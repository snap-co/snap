//! Socket ownership tests, not repeats of Host's controlled-clock lifecycle suite.
use snap_document::runtime::Runtime as Host;
use snap_transport::operation::Definition as Request;
use snap_transport::{Command, Event, Invocation, Response, binary, json};
use snap_transport_native::{Dispatcher, Shared};
use snap_transport_tcp::Client;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
#[path = "../../../crates/platform/transport-tcp/tests/support/mod.rs"]
mod tls_support;

#[tokio::test]
#[ignore = "real TCP adapter"]
async fn adjacent_handshake_streamed_observations_and_new_submission_after_detach() {
    let migrations = [snap_access::MIGRATION, snap_document::server::MIGRATION]
        .map(|s| toml::from_str(s).unwrap());
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    let document =
        snap_document::server::Document::new(snap_document::Registry::new(vec![]).unwrap());
    let executions = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let count = executions.clone();
    let operations = snap_transport::operation::Registry::default().with_request(Request {
        name: "fixture.probe".into(),
        identity_required: true,
        input: |v| v.is_null(),
        output: |v| v.is_u64(),
        progress: |_| false,
        error: |_| true,
        guards: vec![],
        inputs: &[],
        data: snap_store::Data::new(&[]),
        handler: snap_transport::operation::Handler::new(move |_, _, _, _, _| {
            Ok(json!(
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
            ))
        }),
    });
    let host = Host::new(
        store,
        Arc::new(document),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(|_, b| {
            if b == "token" {
                Ok("actor".into())
            } else {
                Err(snap_store::Error::NotFound)
            }
        }))),
        Default::default(),
        "boot".into(),
    );
    let shared = Shared::new(host);
    // TLS permits a wildcard listener; peers still verify the concrete address.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port()));
    let temp = tempfile::tempdir().unwrap();
    let (server_tls, client_tls) = tls_support::pki(temp.path(), false);
    let serving = tokio::spawn(snap_transport_tcp::serve(
        listener,
        Dispatcher::tcp(shared.clone(), None),
        server_tls,
    ));
    let dispatch = tokio::spawn(snap_transport_native::dispatch(shared.clone()));
    let make = || {
        Command::Invoke(Invocation {
            id: 1,
            operation: "fixture.probe".into(),
            input: serde_json::Value::Null,
        })
    };
    let connect = Command::Connect {
        bearer: "token".into(),
        client_id: "stable".into(),
    };
    let mut socket = client_tls.connect(&addr.to_string()).await.unwrap();
    let mut bytes = binary::command(&connect).unwrap();
    bytes.extend(binary::command(&make()).unwrap());
    // Fragment the first header, then combine its remainder with MESSAGE.
    socket.write_all(&bytes[..3]).await.unwrap();
    socket.write_all(&bytes[3..]).await.unwrap();
    socket.flush().await.unwrap();
    let (reply, retention) = snap_transport_tcp::read_response(&mut socket)
        .await
        .unwrap();
    assert_eq!(reply, Response::Attached { resumed: false });
    let attachment = retention.unwrap();
    assert_eq!(attachment.retention_ms, 300000);
    assert!(!attachment.lifetime.is_empty());
    async fn completion(socket: &mut snap_transport_tcp::tls::ClientStream) {
        let mut accepted = false;
        loop {
            let (reply, _) = snap_transport_tcp::read_response(socket).await.unwrap();
            // One event per frame now, so each read is a single observation.
            if let Response::Event(event) = reply {
                {
                    match event {
                        Event::Accepted { id: 1 } => accepted = true,
                        Event::Completed { id: 1, outcome } => {
                            assert!(accepted);
                            assert_eq!(outcome.unwrap(), json!(1));
                            return;
                        }
                        _ => panic!("Unexpected event"),
                    }
                }
            }
        }
    }
    completion(&mut socket).await;
    drop(socket);
    // Host controls provide synchronization rather than sleeps after socket EOF.
    let mut resumed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut c = Client::open(&addr.to_string(), &client_tls).await.unwrap();
            c.send(&connect).await.unwrap();
            let (reply, info) = c.receive().await.unwrap();
            match reply {
                Response::Attached { resumed: true } => {
                    assert_eq!(info.as_ref(), Some(&attachment));
                    break c;
                }
                Response::Failed(snap_transport::Error::Occupied) => tokio::task::yield_now().await,
                other => panic!("{other:?}"),
            }
        }
    })
    .await
    .unwrap();
    resumed.send(&make()).await.unwrap();
    loop {
        let (response, _) = resumed.receive().await.unwrap();
        if matches!(
            response,
            Response::Event(Event::Completed { outcome: Ok(v), .. }) if v == json!(2)
        ) {
            break;
        }
    }
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 2);
    let mut invalid = Client::open(&addr.to_string(), &client_tls).await.unwrap();
    invalid
        .send(&Command::Connect {
            bearer: "bad".into(),
            client_id: "bad".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        invalid.receive().await.unwrap().0,
        Response::Failed(snap_transport::Error::InvalidBearer)
    );
    serving.abort();
    dispatch.abort();
    let _ = serving.await;
    let _ = dispatch.await;
}

#[tokio::test]
async fn connectionless_tcp_returns_bearer_as_a_private_correlated_packet() {
    use snap_transport::bearer::{Change, Receiver, Token};
    let migrations = [snap_access::MIGRATION, snap_document::server::MIGRATION]
        .map(|source| toml::from_str(source).unwrap());
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    let document =
        snap_document::server::Document::new(snap_document::Registry::new(vec![]).unwrap());
    document.metadata().prepare(&mut store).unwrap();
    let operations =
        snap_transport::operation::Registry::default().with_preconnection_request(Request {
            name: "fixture.acquire".into(),
            identity_required: false,
            input: |v| v.is_null(),
            output: |v| v.is_null(),
            progress: |_| false,
            error: |_| true,
            guards: vec![],
            inputs: &[],
            data: snap_store::Data::default(),
            handler: snap_transport::operation::Handler::new(|_, _, _, _, context| {
                context
                    .bearer_changed(Change::Set(Token::new("private-token".into())))
                    .unwrap();
                Ok(json!(null))
            }),
        });
    let host = Host::new(
        store,
        Arc::new(document),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(|_, _| {
            Err(snap_store::Error::NotFound)
        }))),
        Default::default(),
        "bearer-boot".into(),
    );
    let shared = Shared::new(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let (server_tls, client_tls) = tls_support::pki(directory.path(), false);
    let serving = tokio::spawn(snap_transport_tcp::serve(
        listener,
        Dispatcher::tcp(shared, None),
        server_tls,
    ));
    for (operation, success) in [("fixture.acquire", true), ("fixture.unknown", false)] {
        let mut socket = Client::open(&address.to_string(), &client_tls)
            .await
            .unwrap();
        socket
            .send(&Command::Request {
                bearer: None,
                invocation: Invocation {
                    id: 7,
                    operation: operation.into(),
                    input: json!(null),
                },
            })
            .await
            .unwrap();
        let (response, attachment) =
            tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive())
                .await
                .unwrap()
                .unwrap();
        assert!(attachment.is_none());
        if success {
            // A preconnection credential issue is three frames now instead of one
            // batch, published in order and sealed by the completion. The issued
            // token is never observable to the connection's later frames.
            assert!(
                matches!(response, Response::Event(Event::Accepted { id: 7 })),
                "acceptance is published first: {response:?}"
            );
            let (response, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive())
                    .await
                    .unwrap()
                    .unwrap();
            let Response::Event(Event::Bearer {
                id: 7,
                change: Change::Set(token),
            }) = response
            else {
                panic!("credential must be its own frame: {response:?}");
            };
            assert_eq!(token.expose(), "private-token");
            let (response, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive())
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(
                response,
                Response::Event(Event::Completed {
                    id: 7,
                    outcome: Ok(snap_transport::Value::Null),
                })
            );
            // Retirement closed the channel, so nothing further crosses it.
            assert!(
                socket.receive().await.is_err(),
                "retirement must close the channel"
            );
        } else {
            assert_eq!(
                response,
                Response::Failed(snap_transport::Error::UnknownOperation)
            );
        }
    }
    serving.abort();
    let _ = serving.await;
}
