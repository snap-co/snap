//! Native server-carrier fixtures. Queues supply dependency IO, never module
//! outcomes. The conformance cases live in the portable library.
use futures_util::{SinkExt, StreamExt};
use snap_platform_tests::transport::{Duplex, Server};
use snap_transport::{
    Command, Error, Invocation, Response, binary,
    carrier::{AttachmentInfo, Connection, Dispatch, Frame, Submission},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};
#[path = "../../../../crates/platform/transport-tcp/tests/support/mod.rs"]
pub(super) mod tls_support;

#[derive(Clone)]
struct Queues(mpsc::UnboundedSender<HostPeer>);
struct HostPeer {
    incoming: mpsc::UnboundedReceiver<Command>,
    outgoing: mpsc::UnboundedSender<Frame>,
    teardown: mpsc::UnboundedReceiver<bool>,
    late_reply: Arc<Mutex<Option<Frame>>>,
}
struct Endpoint {
    incoming: mpsc::UnboundedSender<Command>,
    outgoing: Mutex<mpsc::UnboundedReceiver<Frame>>,
    teardown: mpsc::UnboundedSender<bool>,
    publish: mpsc::UnboundedSender<Frame>,
    late_reply: Arc<Mutex<Option<Frame>>>,
    retired: AtomicBool,
}
impl Connection for Endpoint {
    fn submit(&self, command: Command, _: usize) -> Result<Submission, Error> {
        if matches!(command, Command::Close) {
            let _ = self.teardown.send(true);
            return Ok(Submission::CloseSocket);
        }
        self.incoming
            .send(command)
            .map(|_| Submission::Queued)
            .map_err(|_| Error::Unavailable)
    }
    fn receive(&self) -> Option<Frame> {
        let response = self.outgoing.lock().unwrap().try_recv().ok();
        if response.is_none()
            && let Some(frame) = self.late_reply.lock().unwrap().take()
        {
            // Reproduce the legal interleaving: an empty receive, publication,
            // then retirement before the driver makes its next observation.
            self.publish.send(frame).unwrap();
            self.retired.store(true, Ordering::Release);
        }
        response
    }
    fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }
    fn disconnect(&self) {
        let _ = self.teardown.send(false);
    }
}
impl Dispatch for Queues {
    type Connection = Endpoint;
    async fn open(&self, _: Option<String>, _: usize) -> Result<Endpoint, Error> {
        let (incoming, requests) = mpsc::unbounded_channel();
        let (replies, outgoing) = mpsc::unbounded_channel();
        let (teardown, signals) = mpsc::unbounded_channel();
        let late_reply = Arc::new(Mutex::new(None));
        self.0
            .send(HostPeer {
                incoming: requests,
                outgoing: replies.clone(),
                teardown: signals,
                late_reply: late_reply.clone(),
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(Endpoint {
            incoming,
            outgoing: Mutex::new(outgoing),
            teardown,
            publish: replies,
            late_reply,
            retired: AtomicBool::new(false),
        })
    }
    async fn request(&self, _: Invocation, _: Option<String>) -> snap_transport::bearer::Reply {
        Err(Error::Unavailable).into()
    }
}

#[derive(Clone, Copy)]
pub enum Driver {
    WebSocket,
    Tcp,
}
enum Socket {
    WebSocket(
        Box<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    Tcp(Box<snap_transport_tcp::tls::ClientStream>),
}
impl Socket {
    async fn send(&mut self, command: &Command) {
        match self {
            Self::WebSocket(socket) => socket
                .send(Message::Text(
                    serde_json::to_string(command).unwrap().into(),
                ))
                .await
                .unwrap(),
            Self::Tcp(socket) => socket
                .write_all(&binary::command(command).unwrap())
                .await
                .unwrap(),
        }
    }
    async fn receive(&mut self) -> Result<(Response, Option<AttachmentInfo>), String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            match self {
                Self::WebSocket(socket) => {
                    let message = socket
                        .next()
                        .await
                        .ok_or("WebSocket EOF")?
                        .map_err(|e| e.to_string())?;
                    Ok((
                        serde_json::from_str(message.to_text().map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?,
                        None,
                    ))
                }
                Self::Tcp(socket) => snap_transport_tcp::read_response(socket)
                    .await
                    .map_err(|e| e.to_string()),
            }
        })
        .await
        .expect("carrier failed to write queued output")
    }
    async fn malformed(&mut self) {
        match self {
            Self::WebSocket(socket) => socket.send(Message::Text("not JSON".into())).await.unwrap(),
            Self::Tcp(socket) => socket.write_all(b"BAD!\x01\x01\0\0\0\0\0\0").await.unwrap(),
        }
    }
}
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub async fn start(driver: Driver) -> Setup {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (openings, mut peers) = mpsc::unbounded_channel();
    let queues = Queues(openings);
    let (server, socket) = match driver {
        Driver::WebSocket => {
            let app = snap_transport_ws::router(Arc::new(snap_transport_ws::Service {
                dispatch: queues,
                origin: format!("http://{address}"),
                cookie: None,
                require_cookie: false,
            }));
            let server = Task(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }));
            let (socket, _) = connect_async(format!("ws://{address}/transport"))
                .await
                .unwrap();
            (server, Socket::WebSocket(Box::new(socket)))
        }
        Driver::Tcp => {
            let temp = tempfile::tempdir().unwrap();
            let (server_tls, client_tls) = tls_support::pki(temp.path(), false);
            let server = Task(tokio::spawn(async move {
                snap_transport_tcp::serve(listener, queues, server_tls)
                    .await
                    .unwrap();
            }));
            let socket = client_tls.connect(&address.to_string()).await.unwrap();
            (server, Socket::Tcp(Box::new(socket)))
        }
    };
    let peer = tokio::time::timeout(Duration::from_secs(2), peers.recv())
        .await
        .unwrap()
        .unwrap();
    Setup {
        socket,
        host: peer,
        _server: server,
    }
}

pub struct Setup {
    socket: Socket,
    host: HostPeer,
    _server: Task,
}
impl Duplex for Setup {
    async fn send(&mut self, command: &Command) {
        self.socket.send(command).await;
    }
    async fn incoming(&mut self) -> Command {
        tokio::time::timeout(Duration::from_secs(2), self.host.incoming.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn publish(&mut self, frame: Frame) {
        self.host.outgoing.send(frame).unwrap();
    }
    async fn receive(&mut self) -> Result<(Response, Option<AttachmentInfo>), String> {
        self.socket.receive().await
    }
}
impl Server for Setup {
    async fn malformed(&mut self) {
        self.socket.malformed().await;
    }
    async fn teardown(&mut self) -> bool {
        tokio::time::timeout(Duration::from_secs(2), self.host.teardown.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn pending_command(&mut self) -> bool {
        self.host.incoming.try_recv().is_ok()
    }
    fn retire_after_empty_receive(&mut self, frame: Frame) {
        *self.host.late_reply.lock().unwrap() = Some(frame);
    }
}
