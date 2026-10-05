//! Native server-carrier fixtures. Queues supply dependency IO, never module
//! outcomes. The conformance cases live in the portable library.
//!
//! The connection here is the production [`snap_transport::inbox::Inbox`], so a
//! WebSocket and a TLS socket are proven to feed the same handoff rather than two
//! lookalikes. The only thing this file adds is the retirement race hook, which
//! must sit between the carrier's own observations and so cannot live inside the
//! queue.
use futures_util::{SinkExt, StreamExt};
use snap_platform_tests::transport::{Duplex, Server};
use snap_transport::{
    Command, Error, Invocation, Response, binary,
    carrier::{AttachmentInfo, Connection, Dispatch, Frame, Submission},
    inbox::Inbox,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};
#[path = "../../../../crates/platform/transport-tcp/tests/support/mod.rs"]
pub(super) mod tls_support;

#[derive(Clone)]
struct Queues(mpsc::UnboundedSender<Peer>);

/// Everything both sides of one connection observe. Splitting it this way keeps
/// a single [`Inbox`] per connection: the carrier half and the application half
/// hold the same one, rather than the fixture keeping its own copy of the queue.
#[derive(Clone)]
struct Peer {
    inbox: Arc<Inbox>,
    /// Set when a logical `Close` was handed off, so teardown can report which
    /// of the two events the caller is waiting for.
    closed: Arc<Mutex<bool>>,
    /// Armed by `retire_after_empty_receive`.
    late_reply: Arc<Mutex<Option<Frame>>>,
}

struct Endpoint(Peer);
impl std::ops::Deref for Endpoint {
    type Target = Peer;
    fn deref(&self) -> &Peer {
        &self.0
    }
}
impl Connection for Endpoint {
    fn submit(&self, command: Command, bytes: usize) -> Result<Submission, Error> {
        if matches!(command, Command::Close) {
            *self.closed.lock().unwrap() = true;
            // A logical Close asks for physical teardown rather than entering the
            // queue: it is the host's policy decision, not work to execute, and the
            // carrier must still drop the socket afterwards.
            return Ok(Submission::CloseSocket);
        }
        self.inbox.submit(command, bytes)
    }
    fn receive(&self) -> Option<Frame> {
        if let Some(frame) = self.inbox.receive() {
            return Some(frame);
        }
        // Reproduce the legal interleaving: an empty receive, publication, then
        // retirement before the carrier makes its next observation.
        if let Some(frame) = self.late_reply.lock().unwrap().take() {
            self.inbox.publish_frame(frame, 1);
            self.inbox.retire();
        }
        None
    }
    fn retired(&self) -> bool {
        self.inbox.retired()
    }
    fn disconnect(&self) {
        self.inbox.disconnect()
    }
}
impl Dispatch for Queues {
    type Connection = Endpoint;
    async fn open(&self, _: Option<String>, budget: usize) -> Result<Endpoint, Error> {
        let peer = Peer {
            inbox: Arc::new(Inbox::new(budget)),
            closed: Arc::new(Mutex::new(false)),
            late_reply: Arc::new(Mutex::new(None)),
        };
        self.0.send(peer.clone()).map_err(|_| Error::Unavailable)?;
        Ok(Endpoint(peer))
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
        application: peer,
        _server: server,
    }
}

pub struct Setup {
    socket: Socket,
    application: Peer,
    _server: Task,
}
impl Setup {
    /// Next queued command, waiting out the carrier's flush interval.
    async fn next_command(&mut self) -> Command {
        loop {
            if let Some(command) = self.application.inbox.next_command() {
                return command;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
impl Duplex for Setup {
    async fn send(&mut self, command: &Command) {
        self.socket.send(command).await;
    }
    async fn incoming(&mut self) -> Command {
        tokio::time::timeout(Duration::from_secs(2), self.next_command())
            .await
            .expect("carrier failed to hand off a decoded command")
    }
    async fn publish(&mut self, frame: Frame) {
        assert!(
            self.application.inbox.publish_frame(frame, 1),
            "an empty reply direction must accept the frame"
        );
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
        let inbox = self.application.inbox.clone();
        loop {
            // A logical Close is reported once, then the physical disconnect that
            // follows it, matching the carrier's teardown order. The flag is taken
            // rather than read, so a second call waits for the socket to drop
            // instead of re-reporting the same Close.
            if std::mem::replace(&mut *self.application.closed.lock().unwrap(), false) {
                return true;
            }
            if inbox.is_detached() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    fn pending_command(&mut self) -> bool {
        self.application.inbox.queued() > 0
    }
    fn retire_after_empty_receive(&mut self, frame: Frame) {
        *self.application.late_reply.lock().unwrap() = Some(frame);
    }
}
