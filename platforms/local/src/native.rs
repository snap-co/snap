//! Native event-loop adapter. The executable owns Tokio and its LocalSet. This
//! baseline uses length-delimited JSON over TCP; deployments supply secure IO.
use crate::{Peer, Platform};
use snap_transport::{
    Channel, Command, Error, Event, Response,
    server::{Application, Authority},
};
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
pub async fn serve<A: Application, R: Authority + 'static>(
    listener: TcpListener,
    platform: Platform<A, R>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let platform = Rc::new(RefCell::new(platform));
    let clock = Instant::now();
    let mut sweep = tokio::time::interval(std::time::Duration::from_millis(10));
    let mut peers = tokio::task::JoinSet::new();
    let result = loop {
        if *shutdown.borrow() {
            break Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            _ = sweep.tick() => platform.borrow_mut().transport.tick(clock.elapsed().as_millis() as u64),
            Some(_) = peers.join_next(), if !peers.is_empty() => {},
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    if peers.len() >= 1024 { drop(stream); continue; }
                    let platform = platform.clone();
                    peers.spawn_local(async move { let _ = serve_peer(stream, platform, clock).await; });
                }
                Err(error) => break Err(error),
            }
        }
    };
    peers.abort_all();
    while peers.join_next().await.is_some() {}
    result
}

struct Physical<A: Application, R: Authority> {
    peer: Peer,
    platform: Rc<RefCell<Platform<A, R>>>,
    clock: Instant,
}
impl<A: Application, R: Authority> Drop for Physical<A, R> {
    fn drop(&mut self) {
        self.platform
            .borrow_mut()
            .lost(&mut self.peer, self.clock.elapsed().as_millis() as u64);
    }
}
async fn serve_peer<A: Application, R: Authority>(
    mut stream: TcpStream,
    platform: Rc<RefCell<Platform<A, R>>>,
    clock: Instant,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut physical = Physical {
        peer: Peer::default(),
        platform,
        clock,
    };
    loop {
        let bytes = read(&mut stream).await?;
        let command: Command = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        // Queue each acceptance before handler entry. Current handlers are
        // synchronous; the event loop writes the queued frames immediately after.
        let mut frames = Vec::new();
        let response = physical.platform.borrow_mut().receive(
            &mut physical.peer,
            command,
            clock.elapsed().as_millis() as u64,
            |event| {
                frames.push(Response::Events(vec![event]));
            },
        );
        if !matches!(response, Response::Events(_)) {
            frames.push(response);
        }
        for frame in frames {
            write(
                &mut stream,
                &serde_json::to_vec(&frame).map_err(io::Error::other)?,
            )
            .await?;
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
                    if events.len() > 2 {
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
