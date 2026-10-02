//! Testy's native JSON/TCP fixture and client channel. The executable owns Tokio
//! and its LocalSet; this fixture is separate from Snap's TLS/CBOR TCP driver.
use snap_transport::execution;
use snap_transport::execution::{Call, Program};
use snap_transport::execution::{Observation, Peer, Runtime, Submission};
use snap_transport::{Channel, Command, Error, Event, Response, server::Authority};
use std::{cell::RefCell, io, rc::Rc, time::Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

const MAX_FRAME: usize = 64 * 1024;
const IO_SECONDS: u64 = 5;

async fn read(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let size = stream.read_u32().await? as usize;
    if size > MAX_FRAME {
        return Err(io::Error::other("frame too large"));
    }
    let mut bytes = vec![0; size];
    tokio::time::timeout(
        std::time::Duration::from_secs(IO_SECONDS),
        stream.read_exact(&mut bytes),
    )
    .await??;
    Ok(bytes)
}
async fn write(stream: &mut TcpStream, response: &[u8]) -> io::Result<()> {
    if response.len() > MAX_FRAME {
        return Err(io::Error::other("frame too large"));
    }
    tokio::time::timeout(std::time::Duration::from_secs(IO_SECONDS), async {
        stream.write_u32(response.len() as u32).await?;
        stream.write_all(response).await
    })
    .await?
}

/// Runs until the application requests shutdown. Detached residents are swept on
/// a timer even if no further traffic arrives. Shutdown drops all resident state.
/// This fixture's read resolver must return immediate inputs without blocking.
/// Use Transport Runtime's submit/step/supply interface to build an async dependency host.
pub async fn serve<P: Program + 'static, R: Authority + 'static>(
    listener: TcpListener,
    platform: Runtime<P, R>,
    mut shutdown: watch::Receiver<bool>,
    reads: impl Fn(&Call, &str) -> execution::Outcome + 'static,
) -> io::Result<()> {
    let platform = Rc::new(RefCell::new(platform));
    let reads: Reads = Rc::new(reads);
    let clock = Instant::now();
    let mut sweep = tokio::time::interval(std::time::Duration::from_millis(10));
    let mut peers = tokio::task::JoinSet::new();
    let result = loop {
        if *shutdown.borrow() {
            break Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            _ = sweep.tick() => {
                let mut platform = platform.borrow_mut();
                platform.tick(clock.elapsed().as_millis() as u64);
                // Native requests below run to completion with immediate inputs;
                // between requests only queued lifecycle releases remain.
                let observation = platform.step();
                debug_assert!(observation.is_none());
            },
            Some(_) = peers.join_next(), if !peers.is_empty() => {},
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    if peers.len() >= 1024 { drop(stream); continue; }
                    let platform = platform.clone();
                    let reads = reads.clone();
                    peers.spawn_local(async move { let _ = serve_peer(stream, platform, clock, reads).await; });
                }
                Err(error) => break Err(error),
            }
        }
    };
    peers.abort_all();
    while peers.join_next().await.is_some() {}
    result
}

type Reads = Rc<dyn Fn(&Call, &str) -> execution::Outcome>;

struct Physical<P: Program, R: Authority> {
    peer: Peer,
    platform: Rc<RefCell<Runtime<P, R>>>,
    clock: Instant,
}
impl<P: Program, R: Authority> Drop for Physical<P, R> {
    fn drop(&mut self) {
        self.platform
            .borrow_mut()
            .lost(&mut self.peer, self.clock.elapsed().as_millis() as u64);
    }
}
async fn serve_peer<P: Program, R: Authority>(
    mut stream: TcpStream,
    platform: Rc<RefCell<Runtime<P, R>>>,
    clock: Instant,
    reads: Reads,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut physical = Physical {
        peer: Peer::default(),
        platform,
        clock,
    };
    loop {
        let bytes = {
            let reading = read(&mut stream);
            tokio::pin!(reading);
            let mut sweep = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                tokio::select! {
                    result = &mut reading => break result?,
                    _ = sweep.tick() => {
                        if physical.platform.borrow().retired(&physical.peer) { return Ok(()); }
                    }
                }
            }
        };
        let command: Command = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        // Queue each acceptance before handler entry. Current handlers are
        // synchronous; the event loop writes the queued frames immediately after.
        let mut frames = Vec::new();
        {
            let mut platform = physical.platform.borrow_mut();
            if let Submission::Ready(response) = platform.submit(
                &mut physical.peer,
                command,
                clock.elapsed().as_millis() as u64,
            ) {
                frames.push(response);
            }
            while let Some(observation) = platform.step() {
                match observation {
                    Observation::Event { event, bearer, .. } => {
                        if let Some(change) = bearer
                            && let Event::Completed { id, .. } = &event
                        {
                            frames.push(Response::Events(vec![Event::Bearer { id: *id, change }]));
                        }
                        frames.push(Response::Events(vec![event]));
                    }
                    Observation::Need { ticket, key } => {
                        // This native fixture selects immediate host inputs only.
                        // The portable scheduler also supports held/asynchronous
                        // reads, exercised by the memory host via explicit supply.
                        let call = platform.pending_call(ticket).expect("pending read");
                        let result = reads(call, &key);
                        platform
                            .supply(ticket, &key, result)
                            .expect("matching read");
                    }
                }
            }
        }
        for frame in frames {
            write(
                &mut stream,
                &serde_json::to_vec(&frame).map_err(io::Error::other)?,
            )
            .await?;
        }
        if physical.platform.borrow().retired(&physical.peer) {
            return Ok(());
        }
    }
}

pub struct Connection {
    stream: TcpStream,
    usable: bool,
}
impl Connection {
    pub async fn open(address: std::net::SocketAddr) -> io::Result<Self> {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            usable: true,
        })
    }
}
impl Channel for Connection {
    async fn exchange(&mut self, command: Command) -> Result<Response, Error> {
        // A cancelled/failed exchange may leave partial frames on this stream.
        // Never reuse it or silently replay; the caller supplies a fresh channel.
        if !self.usable {
            return Err(Error::Unavailable);
        }
        self.usable = false;
        let result = self.round_trip(command).await;
        if result.is_ok() {
            self.usable = true;
        }
        result
    }
}
impl Connection {
    async fn round_trip(&mut self, command: Command) -> Result<Response, Error> {
        let bytes = serde_json::to_vec(&command).map_err(|_| Error::Protocol)?;
        write(&mut self.stream, &bytes)
            .await
            .map_err(|_| Error::Unavailable)?;
        let mut events = Vec::new();
        loop {
            let bytes = tokio::time::timeout(
                std::time::Duration::from_secs(IO_SECONDS),
                read(&mut self.stream),
            )
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(|_| Error::Unavailable)?;
            let response: Response = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
            match response {
                Response::Events(batch) => {
                    let complete = batch
                        .iter()
                        .any(|event| matches!(event, Event::Completed { .. }));
                    events.extend(batch);
                    if events.len() > 3 {
                        return Err(Error::Protocol);
                    }
                    if complete {
                        return Ok(Response::Events(events));
                    }
                }
                other if events.is_empty() => return Ok(other),
                _ => return Err(Error::Protocol),
            }
        }
    }
}
