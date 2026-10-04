//! Existing native handoff regressions, retained during carrier consolidation.
use snap_transport::native::legacy::{Dispatcher, Endpoint, Prepare, Shared};
use snap_transport::{
    Command, Error, Event, Invocation, Response,
    carrier::{Connection, Dispatch, Frame, Submission},
    json,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
type Host<B> = snap_transport::host::Blocking<B, snap_transport::host::Application<B>>;

fn fixture() -> Arc<Shared<Host<snap_store_sqlite::Sqlite>>> {
    Shared::new(host_fixture(snap_document::server::Document::new(
        snap_document::Registry::new(vec![]).unwrap(),
    )))
}
fn host_fixture(document: snap_document::server::Document) -> Host<snap_store_sqlite::Sqlite> {
    let migrations = [
        snap_store::resource::MIGRATION,
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
    ]
    .map(|source| toml::from_str(source).unwrap());
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
    {
        store.load(table).unwrap();
    }
    let document = Arc::new(document);
    let mut operations = snap_transport::operation::Registry::default();
    for definition in snap_document::operations::definitions(document.clone()) {
        operations = operations.with_request(definition);
    }
    Host::new(
        store,
        snap_transport::host::Application::new(vec![snap_document::sync::binding(document)]),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(|_, _| {
            Ok("actor".into())
        }))),
        Default::default(),
        "boot".into(),
    )
}

#[tokio::test]
async fn maintenance_retirement_seals_output_while_controller_finishes_and_document_recovers() {
    use snap_document::{
        Definition, Intent, Mutation, Registry, ServerMessage, Snapshot, server::Document,
    };
    const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";
    let document = || {
        Document::new(
            Registry::new(vec![Definition {
                kind: "counter".into(),
                version: "1".into(),
                validate: |value| value.is_i64(),
                mutations: vec![Mutation {
                    name: "add".into(),
                    minimum: snap_access::Role::Editor,
                    guard: None,
                    apply: |value, args, _| {
                        Ok(json!(value.as_i64().unwrap() + args.as_i64().unwrap()))
                    },
                }],
            }])
            .unwrap(),
        )
    };
    let mut host = host_fixture(document());
    let document = document();
    host.transact("seed", |tx| {
        document.create(
            tx,
            &Snapshot {
                id: ID.into(),
                kind: "counter".into(),
                version: "1".into(),
                revision: 1,
                value: json!(0),
            },
            snap_access::Audience::Restricted,
            "actor",
        )
    })
    .unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let mut entered = Some(entered);
    let (finish, released) = std::sync::mpsc::channel::<()>();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let host = host.map_participant(|documents| {
        documents.with_controller(snap_transport::host::Controller::new(
            "counter",
            snap_document::server::TABLES[0],
            |row| row.get("kind") == Some(&"counter".into()),
            move |ctx, _| {
                count.fetch_add(1, Ordering::SeqCst);
                ctx.progress(json!("before retirement"))?;
                entered.take().unwrap().send(()).unwrap();
                let _ = released.recv_timeout(Duration::from_secs(10));
                ctx.progress(json!("after retirement"))?;
                Ok(())
            },
        ))
    });
    let shared = Shared::new(host);
    let preparation = Arc::new(AtomicUsize::new(0));
    let prepare: Prepare = Arc::new(move |_| {
        let first = preparation.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move {
            if first {
                Ok(())
            } else {
                Err(Error::InvalidBearer)
            }
        })
    });
    let channel = Dispatcher::tcp(shared.clone(), Some(prepare))
        .open(None, 4096)
        .await
        .unwrap();
    let connect = || Command::Connect {
        bearer: "token".into(),
        client_id: "retained".into(),
    };
    channel.submit(connect(), 1).unwrap();
    assert_eq!(
        next_frame(&channel).await.response,
        Response::Attached { resumed: false }
    );
    let invocation = Invocation {
        id: 1,
        operation: "document.mutate".into(),
        input: serde_json::to_value(Intent {
            id: 1,
            document: ID.into(),
            version: "1".into(),
            mutation: "add".into(),
            args: json!(1),
        })
        .unwrap(),
    };
    channel
        .submit(Command::Invoke(invocation.clone()), 1)
        .unwrap();
    assert_eq!(
        next_frame(&channel).await.response,
        Response::Event(Event::Accepted { id: 1 })
    );
    let executing = shared.clone();
    let worker = std::thread::spawn(move || executing.host.lock().unwrap().step());
    tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    // Advance maintenance while the real controller holds the execution gate.
    tokio::time::pause();
    // The maintenance sleep can start after the first advance. Bound virtual
    // advances by a wall deadline so the regression cannot hang the runner.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !channel.retired() && std::time::Instant::now() < deadline {
        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::task::yield_now().await;
    }
    let retired = channel.retired();
    let controller_still_running = !worker.is_finished();
    let final_frames: Vec<_> = std::iter::from_fn(|| channel.receive()).collect();
    // Release before assertions so a red regression cannot strand the worker.
    drop(finish);
    assert!(worker.join().unwrap());
    tokio::time::resume();
    assert!(retired, "maintenance did not retire the physical output");
    let late: Vec<_> = std::iter::from_fn(|| channel.receive()).collect();
    assert!(
        controller_still_running,
        "retirement waited for controller execution"
    );
    assert!(
        late.is_empty(),
        "retired physical outbox published late controller output: {late:?}"
    );
    assert!(final_frames.iter().any(|frame| frame.response
        == Response::Event(Event::Progress {
            id: 1,
            value: json!("before retirement")
        })));
    let final_frame = final_frames.last().unwrap();
    assert_eq!(final_frame.response, Response::Failed(Error::InvalidBearer));
    assert!(final_frame.terminal);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        shared
            .host
            .lock()
            .unwrap()
            .transact("read commit", |tx| document.retained(tx, ID))
            .unwrap()
            .value,
        json!(1)
    );
    // A new invocation recovers Document's receipt without rerunning the mutation.
    channel.disconnect();
    let reopened = Dispatcher::web(shared.clone())
        .open(None, 4096)
        .await
        .unwrap();
    reopened.submit(connect(), 1).unwrap();
    assert_eq!(
        next_frame(&reopened).await.response,
        Response::Attached { resumed: true }
    );
    assert!(reopened.receive().is_none());
    reopened.submit(Command::Invoke(invocation), 1).unwrap();
    assert_eq!(
        next_frame(&reopened).await.response,
        Response::Event(Event::Accepted { id: 1 })
    );
    assert!(shared.host.lock().unwrap().step());
    let value = loop {
        match next_frame(&reopened).await.response {
            Response::Event(Event::Completed {
                id: 1,
                outcome: Ok(value),
            }) => break value,
            Response::Global { .. } => {}
            other => panic!("expected recovered Document completion, got {other:?}"),
        }
    };
    let ServerMessage::Completed(completion) = serde_json::from_value(value).unwrap() else {
        panic!("not a Document completion");
    };
    assert_eq!(completion.result.unwrap().unwrap().value, json!(1));
    assert!(!shared.host.lock().unwrap().step());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

async fn next_frame(channel: &Endpoint<Host<snap_store_sqlite::Sqlite>>) -> Frame {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(frame) = channel.receive() {
                return frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("host did not publish a frame")
}

#[tokio::test]
async fn handoff_and_teardown_do_not_wait_for_the_execution_gate() {
    let shared = fixture();
    let dispatcher = Dispatcher::web(shared.clone());
    let channel = Arc::new(dispatcher.open(None, 4).await.unwrap());
    // Holding the real gate must not stop the carrier. No mock supplies ordering.
    let gate = shared.host.lock().unwrap();
    let (finished, waiting) = std::sync::mpsc::channel();
    let socket = channel.clone();
    let thread = std::thread::spawn(move || {
        let command = || Command::Connect {
            bearer: "token".into(),
            client_id: "client".into(),
        };
        let oversized = socket.submit(command(), 5);
        let queued = socket.submit(command(), 4);
        let output = socket.receive();
        let close = socket.submit(Command::Close, 0);
        socket.disconnect();
        finished.send((oversized, queued, output, close)).unwrap();
    });
    let result = waiting.recv_timeout(Duration::from_secs(2));
    drop(gate);
    thread.join().unwrap();
    let (oversized, queued, output, close) =
        result.expect("carrier waited for application execution");
    assert_eq!(oversized, Err(Error::Capacity));
    assert_eq!(queued, Ok(Submission::Queued));
    assert_eq!(close, Ok(Submission::CloseSocket));
    assert!(output.is_none());
    tokio::time::timeout(Duration::from_secs(2), async {
        while !channel.retired() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        channel.receive().is_none(),
        "closed queued command reached admission"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_held_commands_keep_byte_and_count_reservations() {
    for (budget, bytes, additional) in [(4, 4, 0), (usize::MAX, 1, 1023)] {
        let shared = fixture();
        let (preparing, entered) = std::sync::mpsc::channel();
        let prepare: Prepare = Arc::new(move |_| {
            let preparing = preparing.clone();
            Box::pin(async move {
                preparing.send(()).unwrap();
                Ok(())
            })
        });
        let channel = Dispatcher::tcp(shared.clone(), Some(prepare))
            .open(None, budget)
            .await
            .unwrap();
        let command = || Command::Connect {
            bearer: "token".into(),
            client_id: "queued".into(),
        };
        let gate = shared.host.lock().unwrap();
        assert_eq!(channel.submit(command(), bytes), Ok(Submission::Queued));
        // The production preparation callback proves the worker took the command.
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..additional {
            assert_eq!(channel.submit(command(), bytes), Ok(Submission::Queued));
        }
        let overflow = channel.submit(command(), bytes);
        channel.submit(Command::Close, 0).unwrap();
        channel.disconnect();
        drop(gate);
        assert_eq!(overflow, Err(Error::Capacity));
    }
}
