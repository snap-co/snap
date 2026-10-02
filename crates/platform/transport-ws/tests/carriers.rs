//! One socket-level contract for both independently selectable drivers. The host
//! fixture only provides queues, so these tests need no Document/Store/Identity.
use futures_util::{SinkExt, StreamExt};
use snap_transport::{
    Command, Error, Event, Invocation, Response, binary,
    carrier::{AttachmentInfo, Connection, Dispatch, Frame, Submission},
    json,
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
#[path = "../../transport-tcp/tests/support/mod.rs"]
mod tls_support;

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
enum Driver {
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
    async fn receive(&mut self) -> (Response, Option<AttachmentInfo>) {
        tokio::time::timeout(Duration::from_secs(2), async {
            match self {
                Self::WebSocket(socket) => {
                    let message = socket.next().await.unwrap().unwrap();
                    (
                        serde_json::from_str(message.to_text().unwrap()).unwrap(),
                        None,
                    )
                }
                Self::Tcp(socket) => snap_transport_tcp::read_response(socket).await.unwrap(),
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
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn start(driver: Driver) -> (Server, Socket, HostPeer) {
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
            let server = Server(tokio::spawn(async move {
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
            let server = Server(tokio::spawn(async move {
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
    (server, socket, peer)
}

#[tokio::test]
async fn drivers_handoff_commands_and_write_observations_without_waiting_for_execution() {
    for driver in [Driver::WebSocket, Driver::Tcp] {
        let (_server, mut socket, mut host) = start(driver).await;
        let connect = Command::Connect {
            bearer: "private".into(),
            client_id: "client".into(),
        };
        socket.send(&connect).await;
        let command = tokio::time::timeout(Duration::from_secs(2), host.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            serde_json::to_value(&connect).unwrap()
        );
        let attachment = AttachmentInfo {
            retention_ms: 123,
            lifetime: "boot:connection".into(),
        };
        host.outgoing
            .send(Frame {
                response: Response::Attached { resumed: false },
                handshake: true,
                attachment: Some(attachment.clone()),
                terminal: false,
            })
            .unwrap();
        let (response, info) = socket.receive().await;
        assert_eq!(response, Response::Attached { resumed: false });
        if matches!(driver, Driver::Tcp) {
            assert_eq!(info, Some(attachment));
        }
        let invoke = Command::Invoke(Invocation {
            id: 7,
            operation: "probe.run".into(),
            input: json!({"value":3}),
        });
        socket.send(&invoke).await;
        let command = tokio::time::timeout(Duration::from_secs(2), host.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            serde_json::to_value(invoke).unwrap()
        );
        // The application has not completed the invocation. Independent output
        // still crosses an otherwise idle socket, including TCP's pinned read.
        let progress = Response::Events(vec![Event::Progress {
            id: 7,
            value: json!("waiting"),
        }]);
        host.outgoing
            .send(Frame {
                response: progress.clone(),
                handshake: false,
                attachment: None,
                terminal: false,
            })
            .unwrap();
        assert_eq!(socket.receive().await.0, progress);
        socket.send(&Command::Close).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), host.teardown.recv())
                .await
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), host.teardown.recv())
                .await
                .unwrap(),
            Some(false)
        );
    }
}

#[tokio::test]
async fn malformed_requests_disconnect_without_reaching_dispatch() {
    for driver in [Driver::WebSocket, Driver::Tcp] {
        let (_server, mut socket, mut host) = start(driver).await;
        socket.malformed().await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), host.teardown.recv())
                .await
                .unwrap(),
            Some(false)
        );
        assert!(host.incoming.try_recv().is_err());
    }
}

async fn final_reply_at_retirement(driver: Driver) {
    let (_server, mut socket, host) = start(driver).await;
    let response = Response::Events(vec![Event::Completed {
        id: 9,
        outcome: Ok(json!("committed")),
    }]);
    *host.late_reply.lock().unwrap() = Some(Frame {
        response: response.clone(),
        handshake: false,
        attachment: None,
        terminal: true,
    });
    assert_eq!(socket.receive().await.0, response);
}
#[tokio::test]
async fn websocket_writes_reply_published_at_retirement() {
    final_reply_at_retirement(Driver::WebSocket).await;
}
#[tokio::test]
async fn tcp_writes_reply_published_at_retirement() {
    final_reply_at_retirement(Driver::Tcp).await;
}
