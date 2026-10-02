//! Native host boundaries: physical peer release, ingress reservations and
//! socket progress while cooperative host work holds the execution gate.
use super::*;
use futures_util::{SinkExt, StreamExt};
use snap_transport::{
    Command, Error, Invocation, Response, Value,
    carrier::{Connection, Dispatch, Submission},
    execution::{Admission, Attempt, Call, Executor, Operation, Runtime, View, WorkingSet},
    server::{Config, Server},
};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct Empty;
impl Program for Empty {
    fn state_version(&self) -> u64 {
        1
    }
    fn valid_state(&self, _: &Value) -> bool {
        true
    }
    fn operations(&self) -> &[Operation] {
        &[]
    }
    fn admit(&self, _: &Call, _: View<'_>) -> Admission {
        Admission::Ready
    }
    fn attempt(&self, _: &Call, _: WorkingSet<'_>) -> Attempt {
        Attempt::Fail(Error::UnknownOperation)
    }
}
struct Auth(Arc<AtomicBool>);
impl Authority for Auth {
    fn identify(&self, _: &str) -> Result<String, Error> {
        if self.0.load(Ordering::Acquire) {
            Ok("actor".into())
        } else {
            Err(Error::InvalidBearer)
        }
    }
}
fn runtime(valid: Arc<AtomicBool>) -> Runtime<Empty, Auth> {
    Runtime::new(
        Server::new(Auth(valid), Config::default()).with_live_authority(),
        Executor::new(Empty, 128).unwrap(),
    )
}
fn development(runtime: Runtime<Empty, Auth>) -> Development<Empty, Auth> {
    Development::new(runtime, |_, _| Err(Error::Unavailable), |_| Some(Empty))
}
struct Stop(tokio::task::JoinHandle<std::io::Result<()>>);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn start(host: Development<Empty, Auth>) -> (Stop, std::net::SocketAddr) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        Stop(tokio::spawn(serve(listener, host, String::new()))),
        address,
    )
}
async fn inspect_peers(address: std::net::SocketAddr) -> Value {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET /__dev HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let text = String::from_utf8(bytes).unwrap();
    serde_json::from_str::<Value>(text.split_once("\r\n\r\n").unwrap().1).unwrap()["peers"].clone()
}

#[tokio::test]
async fn natural_retirement_releases_the_physical_peer_and_full_reopen_capacity() {
    let valid = Arc::new(AtomicBool::new(true));
    let (_server, address) = start(development(runtime(valid.clone()))).await;
    let (mut socket, _) = connect_async(format!("ws://{address}/transport"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&Command::Connect {
                bearer: "token".into(),
                client_id: "retiring".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let response = socket.next().await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Response>(response.to_text().unwrap()).unwrap(),
        Response::Attached { resumed: false }
    );
    valid.store(false, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(inspect_peers(address).await, json!([]));
    valid.store(true, Ordering::Release);
    let mut reopened = Vec::new();
    for _ in 0..128 {
        let (socket, _) = connect_async(format!("ws://{address}/transport"))
            .await
            .expect("retired peer consumed a physical slot");
        reopened.push(socket);
    }
    assert_eq!(inspect_peers(address).await.as_array().unwrap().len(), 128);
}

#[tokio::test]
async fn a_worker_waiting_for_admission_keeps_its_ingress_reservations() {
    for (budget, bytes, additional) in [(4, 4, 0), (usize::MAX, 1, 1023)] {
        let host = development(runtime(Arc::new(AtomicBool::new(true))));
        let (updates, _) = watch::channel(debugger::Report::new(host.inspect()));
        let shared = Arc::new(Shared {
            host: Mutex::new(host),
            clock: Instant::now(),
            authority: "http://localhost".into(),
            updates,
        });
        let channel = carrier::Dispatcher(shared.clone())
            .open(None, budget)
            .await
            .unwrap();
        let (held, entered) = std::sync::mpsc::channel();
        let (release, waiting) = std::sync::mpsc::channel::<()>();
        let execution = std::thread::spawn(move || {
            let _gate = shared.host.lock().unwrap();
            held.send(()).unwrap();
            let _ = waiting.recv();
        });
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let command = || Command::Connect {
            bearer: "token".into(),
            client_id: "queued".into(),
        };
        assert_eq!(channel.submit(command(), bytes), Ok(Submission::Queued));
        // Give the real worker a scheduling turn to take the first item and wait
        // for the held gate. The reservation must survive that handoff.
        tokio::time::sleep(Duration::from_millis(50)).await;
        for _ in 0..additional {
            assert_eq!(channel.submit(command(), bytes), Ok(Submission::Queued));
        }
        let overflow = channel.submit(command(), bytes);
        channel.disconnect();
        drop(release);
        execution.join().unwrap();
        assert_eq!(overflow, Err(Error::Capacity));
    }
}

#[tokio::test]
async fn socket_executor_progresses_while_host_preparation_holds_the_gate() {
    let settled = Arc::new(AtomicBool::new(false));
    let finished = settled.clone();
    let (entered, waiting) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let mut prepare = Some((entered, released));
    let runtime = runtime(Arc::new(AtomicBool::new(true))).with_requests(
        |name| name == "fixture.held",
        move |_, _| {
            let (entered, released) = prepare.take().unwrap();
            let finished = finished.clone();
            Some(Ok(Box::new(move || {
                let _ = entered.send(());
                let _ = released.recv_timeout(Duration::from_secs(5));
                finished.store(true, Ordering::Release);
                Ok(Value::Null).into()
            })))
        },
    );
    let (_server, address) = start(development(runtime)).await;
    let (mut socket, _) = connect_async(format!("ws://{address}/transport"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&Command::Request {
                bearer: None,
                invocation: Invocation {
                    id: 1,
                    operation: "fixture.held".into(),
                    input: Value::Null,
                },
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    waiting.await.unwrap();
    // Cross the real 50 ms sweep while the host callback remains held. A finite
    // callback deadline ensures even the pre-fix executor stall cannot hang tests.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let progressed = !settled.load(Ordering::Acquire);
    if progressed {
        socket
            .send(Message::Ping(vec![1, 2, 3].into()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Message::Pong(bytes) = socket
                    .next()
                    .await
                    .expect("socket closed during host preparation")
                    .unwrap()
                {
                    assert_eq!(&bytes[..], &[1, 2, 3]);
                    break;
                }
            }
        })
        .await
        .expect("socket could not respond while host work was held");
    }
    drop(release);
    assert!(
        progressed,
        "host sweep blocked the socket executor until preparation finished"
    );
}
