//! Socket ownership tests, not repeats of Host's controlled-clock lifecycle suite.
use snap_document_local::{Host, web::Shared};
use snap_transport::operation::Definition as Request;
use snap_transport::{Command, Event, Invocation, Response, binary, json};
use snap_transport_native::Client;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
#[path = "../../transport/tests/support/mod.rs"]
mod tls_support;

#[tokio::test]
#[ignore = "real TCP adapter"]
async fn adjacent_handshake_streamed_observations_and_detached_replay() {
    let migrations = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
    ]
    .map(|s| toml::from_str(s).unwrap());
    let mut store = snap_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    let document = snap_document::server::Document::new(
        snap_document::Registry::new(vec![]).unwrap(),
        snap_access::Access::new(vec![snap_access::KindDefinition::kind("document").unwrap()])
            .unwrap(),
    );
    let executions = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let count = executions.clone();
    let host = Host::new(
        store,
        document,
        Arc::new(|_, b| {
            if b == "token" {
                Ok("actor".into())
            } else {
                Err(snap_store::Error::NotFound)
            }
        }),
        Default::default(),
        "boot".into(),
    )
    .with_request(Request {
        name: "fixture.probe".into(),
        identity_required: true,
        input: |v| v.is_null(),
        output: |v| v.is_u64(),
        progress: |_| false,
        guards: &[],
        tables: &[],
        handler: Box::new(move |_, _, _, _| {
            Ok(json!(
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
            ))
        }),
    });
    let shared = Shared::new(host, "http://localhost".into());
    // TLS permits a wildcard listener; peers still verify the concrete address.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port()));
    let temp = tempfile::tempdir().unwrap();
    let (server_tls, client_tls) = tls_support::pki(temp.path(), false);
    let serving = tokio::spawn(snap_document_local::tcp::serve(
        listener,
        shared.clone(),
        server_tls,
    ));
    let dispatch = tokio::spawn(snap_document_local::web::dispatch(shared.clone()));
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
    let (reply, retention) = snap_transport_native::read_response(&mut socket)
        .await
        .unwrap();
    assert_eq!(reply, Response::Attached { resumed: false });
    let attachment = retention.unwrap();
    assert_eq!(attachment.retention_ms, 300000);
    assert!(!attachment.lifetime.is_empty());
    async fn completion(socket: &mut snap_transport_native::tls::ClientStream) {
        let mut accepted = false;
        loop {
            let (reply, _) = snap_transport_native::read_response(socket).await.unwrap();
            if let Response::Events(events) = reply {
                for event in events {
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
        if let Response::Events(events) = response
            && events
                .iter()
                .any(|e| matches!(e,Event::Completed {outcome:Ok(v),..} if v==&json!(1)))
        {
            break;
        }
    }
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 1);
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
