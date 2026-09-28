use snap_access::{
    Access, Actor, Audience, ChangeSet, GrantChange, KindDefinition, Resource, Role,
};
use snap_document::{
    Definition, Intent, Manifest, Mutation, Registry, ServerMessage, Snapshot, server::Document,
};
use snap_document_local::Host;
use snap_transport::{Command, Event, Invocation, Response, json, server::Config};
use std::sync::Arc;

const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";

fn access() -> Access {
    Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap()
}

fn registry() -> Registry {
    Registry::new(vec![Definition {
        kind: "counter".into(),
        version: "1".into(),
        validate: |value| value.as_i64().is_some(),
        mutations: vec![Mutation {
            name: "add".into(),
            minimum: Role::Editor,
            guard: None,
            apply: |value, args, _| {
                Ok(json!(
                    value.as_i64().unwrap() + args.as_i64().ok_or(snap_document::Error::Invalid)?
                ))
            },
        }],
    }])
    .unwrap()
}

fn fixture() -> Host<snap_sqlite::Sqlite> {
    let mut migrations: Vec<snap_store::migration::Migration> = vec![
        toml::from_str(snap_access::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::LIFECYCLE_MIGRATION).unwrap(),
    ];
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    let document = Document::new(registry(), access());
    store
        .run("fixture", |tx| {
            document.create(
                tx,
                &Snapshot {
                    id: ID.into(),
                    kind: "counter".into(),
                    version: "1".into(),
                    revision: 1,
                    value: json!(0),
                },
                Audience::Restricted,
                "alice",
            )?;
            let mut changes = ChangeSet::new(Actor::system());
            changes.grants.push(GrantChange {
                resource: Resource::new("document", ID).unwrap(),
                identity: "bob".into(),
                role: Some(Role::Editor),
            });
            access().change(tx, &changes)?;
            Ok(())
        })
        .unwrap();
    Host::new(
        store,
        document,
        Arc::new(|_, bearer| match bearer {
            "alice" | "bob" => Ok(bearer.into()),
            _ => Err(snap_store::Error::NotFound),
        }),
        Config {
            reconnect_ms: 100,
            capacity: 16,
        },
        "fixture-boot".into(),
    )
}

fn connect(
    host: &mut Host<snap_sqlite::Sqlite>,
    actor: &str,
    client: &str,
    now: u64,
) -> (u64, bool) {
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: actor.into(),
            client_id: client.into(),
        },
        now,
    )
    .unwrap();
    let response = host.drain(peer).unwrap();
    let Response::Attached { resumed } = response[0] else {
        panic!("{response:?}")
    };
    (peer, resumed)
}

fn manifest(
    host: &mut Host<snap_sqlite::Sqlite>,
    peer: u64,
    wire: u64,
    pending: Vec<Intent>,
) -> Vec<Response> {
    host.submit(
        peer,
        Command::Invoke(Invocation {
            id: wire,
            operation: "document.manifest".into(),
            input: serde_json::to_value(Manifest {
                holdings: vec![],
                pending,
            })
            .unwrap(),
        }),
        0,
    )
    .unwrap();
    assert!(host.step());
    host.drain(peer).unwrap()
}

fn intent(id: u64, amount: i64) -> Intent {
    Intent {
        id,
        document: ID.into(),
        version: "1".into(),
        mutation: "add".into(),
        args: json!(amount),
    }
}
fn submit(host: &mut Host<snap_sqlite::Sqlite>, peer: u64, wire: u64, intent: Intent) {
    host.submit(
        peer,
        Command::Invoke(Invocation {
            id: wire,
            operation: "document.mutate".into(),
            input: serde_json::to_value(intent).unwrap(),
        }),
        0,
    )
    .unwrap();
}
fn messages(responses: Vec<Response>) -> Vec<ServerMessage> {
    responses
        .into_iter()
        .flat_map(|response| match response {
            Response::Events(events) => events
                .into_iter()
                .filter_map(|event| match event {
                    Event::Completed {
                        outcome: Ok(value), ..
                    } => Some(serde_json::from_value(value).unwrap()),
                    _ => None,
                })
                .collect(),
            Response::Notification { input, .. } => match serde_json::from_value(input).unwrap() {
                ServerMessage::Committed(_) => vec![],
                message => vec![message],
            },
            _ => vec![],
        })
        .collect()
}

#[test]
fn later_submissions_wait_for_the_accepted_operation_before_ack() {
    let mut host = fixture();
    let (alice, _) = connect(&mut host, "alice", "a", 0);
    let (bob, _) = connect(&mut host, "bob", "b", 0);
    manifest(&mut host, alice, 1, vec![]);
    manifest(&mut host, bob, 1, vec![]);
    host.drain(alice).unwrap();
    submit(&mut host, alice, 2, intent(1, 1));
    assert!(
        matches!(host.drain(alice).unwrap().as_slice(), [Response::Events(events)] if events == &vec![Event::Accepted{id:2}])
    );
    submit(&mut host, alice, 3, intent(2, 2));
    assert!(host.drain(alice).unwrap().is_empty());
    assert!(host.step());
    let first = messages(host.drain(alice).unwrap());
    assert!(
        matches!(&first[0],ServerMessage::Completed(c) if c.result.as_ref().unwrap().as_ref().unwrap().value == json!(1))
    );
    assert!(host.step());
    let second = messages(host.drain(alice).unwrap());
    assert!(
        matches!(&second[0],ServerMessage::Completed(c) if c.result.as_ref().unwrap().as_ref().unwrap().value == json!(3))
    );
    let remote = messages(host.drain(bob).unwrap());
    assert_eq!(
        remote
            .iter()
            .filter(|m| matches!(m, ServerMessage::Replication(_)))
            .count(),
        2
    );
    assert!(!host.step());
}

#[test]
fn revocation_waits_for_accepted_writes_and_preserves_their_completion() {
    let mut host = fixture();
    let (alice, _) = connect(&mut host, "alice", "a", 0);
    let (bob, _) = connect(&mut host, "bob", "b", 0);
    manifest(&mut host, alice, 1, vec![]);
    manifest(&mut host, bob, 1, vec![]);
    submit(&mut host, alice, 2, intent(1, 3));
    host.step();
    submit(&mut host, bob, 2, intent(1, 100));
    host.transact("revoke", |tx| {
        let mut change = ChangeSet::new(Actor::system());
        change.grants.push(GrantChange {
            resource: Resource::new("document", ID).unwrap(),
            identity: "bob".into(),
            role: None,
        });
        access().change(tx, &change).map(|_| ())
    })
    .unwrap();
    host.step();
    let remote = messages(host.drain(bob).unwrap());
    assert!(
        !remote
            .iter()
            .any(|m| matches!(m, ServerMessage::Replication(_)))
    );
    assert!(
        remote
            .iter()
            .any(|m| matches!(m,ServerMessage::Removed(ids) if ids==&vec![ID.to_owned()]))
    );
    assert!(
        remote
            .iter()
            .any(|m| matches!(m,ServerMessage::Completed(c) if c.result.as_ref().unwrap().as_ref().unwrap().value == json!(103)))
    );
    let state = messages(manifest(&mut host, alice, 3, vec![]));
    assert!(state.iter().any(
        |m| matches!(m,ServerMessage::Manifest(state) if state.documents[0].value==json!(103))
    ));
}

#[test]
fn interrupted_completion_recovers_once_only_inside_surviving_lifetime() {
    let mut host = fixture();
    let (alice, resumed) = connect(&mut host, "alice", "a", 0);
    assert!(!resumed);
    manifest(&mut host, alice, 1, vec![]);
    submit(&mut host, alice, 2, intent(1, 7));
    host.step();
    host.lost(alice, 10);
    let (replacement, resumed) = connect(&mut host, "alice", "a", 20);
    assert!(resumed);
    let recovery = messages(manifest(&mut host, replacement, 3, vec![intent(1, 7)]));
    assert!(recovery.iter().any(|m|matches!(m,ServerMessage::Manifest(state) if state.completed.len()==1 && state.documents[0].value==json!(7))));
    submit(&mut host, replacement, 2, intent(1, 7));
    host.step();
    let repeated = messages(host.drain(replacement).unwrap());
    assert!(repeated.iter().any(|m|matches!(m,ServerMessage::Completed(c) if c.result.as_ref().unwrap().as_ref().unwrap().value==json!(7))));
    host.lost(replacement, 30);
    host.tick(130);
    let (fresh, resumed) = connect(&mut host, "alice", "a", 131);
    assert!(!resumed);
    let recovery = messages(manifest(&mut host, fresh, 1, vec![]));
    assert!(recovery.iter().any(|m|matches!(m,ServerMessage::Manifest(state) if state.completed.is_empty() && state.documents[0].value==json!(7))));
}

#[tokio::test]
#[ignore = "real socket suite"]
async fn websocket_delivers_ack_before_execution_and_serializes_following_acceptance() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shared = snap_document_local::web::Shared::new(fixture(), format!("http://{address}"));
    let router = snap_document_local::web::router(shared.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    // Own and abort only this test's local listener, including on assertion failure.
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _stop = Stop(server);
    let (mut socket, _) = connect_async(format!("ws://{address}/transport"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&Command::Connect {
                bearer: "alice".into(),
                client_id: "real-wire".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        serde_json::from_str::<Response>(message.to_text().unwrap()).unwrap(),
        Response::Attached { resumed: false }
    ));
    for (id, amount) in [(1, 2), (2, 3)] {
        let command = Command::Invoke(Invocation {
            id,
            operation: "document.mutate".into(),
            input: serde_json::to_value(intent(id, amount)).unwrap(),
        });
        socket
            .send(Message::Text(
                serde_json::to_string(&command).unwrap().into(),
            ))
            .await
            .unwrap();
        if id == 2 {
            break;
        }
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Response>(message.to_text().unwrap()).unwrap(),
            Response::Events(vec![Event::Accepted { id }])
        );
    }
    // The first ACK crossed the socket before any handler ran. Drive the first
    // completion; the second command may still be waiting in the carrier reader.
    assert!(shared.host.lock().unwrap().step());
    let _dispatch = Stop(tokio::spawn(snap_document_local::web::dispatch(
        shared.clone(),
    )));
    let mut values = vec![];
    while values.len() < 2 {
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for message in messages(vec![
            serde_json::from_str::<Response>(message.to_text().unwrap()).unwrap(),
        ]) {
            if let ServerMessage::Completed(completion) = message {
                values.push(completion.result.unwrap().unwrap().value);
            }
        }
    }
    assert_eq!(values, vec![json!(2), json!(5)]);
    socket.close(None).await.unwrap();
}

#[test]
fn physical_loss_and_logical_expiry_both_drain_accepted_work() {
    for expire in [false, true] {
        let mut host = fixture();
        let (peer, _) = connect(&mut host, "alice", "a", 0);
        submit(&mut host, peer, 1, intent(1, 5));
        host.lost(peer, 10);
        if expire {
            host.tick(110);
        }
        assert!(host.step());
        let (peer, resumed) = connect(&mut host, "alice", "a", if expire { 111 } else { 20 });
        assert_eq!(resumed, !expire);
        let recovered = messages(manifest(
            &mut host,
            peer,
            2,
            if expire { vec![] } else { vec![intent(1, 5)] },
        ));
        assert!(
            recovered
                .iter()
                .any(|message| matches!(message, ServerMessage::Manifest(state)
            if state.documents[0].value == json!(5)
            && state.completed.len() == usize::from(!expire)))
        );
    }
}

#[tokio::test]
#[ignore = "real socket suite"]
async fn websocket_close_drops_socket_while_controller_io_is_held() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (finish, released) = std::sync::mpsc::channel();
    let mut entered = Some(entered);
    let host = fixture().with_controller(
        "counter",
        Box::new(move |_, _| {
            entered.take().unwrap().send(()).unwrap();
            released
                .recv()
                .map_err(|_| snap_store::Error::Unavailable)?;
            Ok(())
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shared = snap_document_local::web::Shared::new(host, format!("http://{address}"));
    let router = snap_document_local::web::router(shared.clone());
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Stop(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let (mut socket, _) = connect_async(format!("ws://{address}/transport"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&Command::Connect {
                bearer: "alice".into(),
                client_id: "close-held".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let timeout = std::time::Duration::from_secs(2);
    let attached = tokio::time::timeout(timeout, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        serde_json::from_str::<Response>(attached.to_text().unwrap()).unwrap(),
        Response::Attached { .. }
    ));
    socket
        .send(Message::Text(
            serde_json::to_string(&Command::Invoke(Invocation {
                id: 1,
                operation: "document.mutate".into(),
                input: serde_json::to_value(intent(1, 3)).unwrap(),
            }))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let ack = tokio::time::timeout(timeout, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Response>(ack.to_text().unwrap()).unwrap(),
        Response::Events(vec![Event::Accepted { id: 1 }])
    );
    let worker_host = shared.clone();
    let worker = tokio::task::spawn_blocking(move || worker_host.host.lock().unwrap().step());
    tokio::time::timeout(timeout, waiting)
        .await
        .unwrap()
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&Command::Close).unwrap().into(),
        ))
        .await
        .unwrap();
    let closed = tokio::time::timeout(timeout, async {
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
    // Always release the blocking worker before asserting the socket result.
    finish.send(()).unwrap();
    assert!(worker.await.unwrap());
    closed.expect("physical close waited for controller IO");
    let mut host = shared.host.lock().unwrap();
    host.tick(10);
    assert_eq!(host.residency_references(ID), 0);
    assert_eq!(
        host.transact("read drained mutation", |tx| Document::new(
            registry(),
            access()
        )
        .read(tx, ID, Some("alice")))
            .unwrap()
            .value,
        json!(3)
    );
}

#[test]
fn controller_commits_progress_and_converges_before_the_next_acceptance() {
    let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = observations.clone();
    let mut host = fixture().with_controller(
        "counter",
        Box::new(move |ctx, snapshot| {
            log.lock().unwrap().push(snapshot.value.clone());
            if snapshot.value == json!(1) {
                ctx.progress(json!({"message":"preparing"}))?;
                ctx.transact("controller.observe", |tx| {
                    Document::new(registry(), access()).replace(tx, ID, "alice", json!(2))?;
                    Ok(())
                })?;
            }
            Ok(())
        }),
    );
    let (peer, _) = connect(&mut host, "alice", "controller", 0);
    submit(&mut host, peer, 1, intent(1, 1));
    submit(&mut host, peer, 2, intent(2, 10));
    assert_eq!(
        host.drain(peer).unwrap(),
        vec![Response::Events(vec![Event::Accepted { id: 1 }])]
    );
    host.step();
    assert_eq!(*observations.lock().unwrap(), vec![json!(1), json!(2)]);
    let events: Vec<_> = host
        .drain(peer)
        .unwrap()
        .into_iter()
        .flat_map(|response| match response {
            Response::Events(events) => events,
            _ => vec![],
        })
        .collect();
    assert!(matches!(
        &events[..],
        [
            Event::Progress { id: 1, .. },
            Event::Completed {
                id: 1,
                outcome: Ok(_)
            }
        ]
    ));
    host.step();
    assert_eq!(
        *observations.lock().unwrap(),
        vec![json!(1), json!(2), json!(12)]
    );
}

#[test]
fn controller_io_does_not_hold_the_carrier_output_lock() {
    let (entered, waiting) = std::sync::mpsc::channel();
    let (finish, released) = std::sync::mpsc::channel();
    let mut host = fixture().with_controller(
        "counter",
        Box::new(move |ctx, _| {
            ctx.progress(json!("waiting for IO"))?;
            entered.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        }),
    );
    let (peer, _) = connect(&mut host, "alice", "held-io", 0);
    let output = host.output(peer).unwrap();
    submit(&mut host, peer, 1, intent(1, 1));
    let worker = std::thread::spawn(move || {
        host.step();
        host
    });
    waiting
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let mut events = Vec::new();
    while let Some(response) = output.pop_front() {
        if let Response::Events(batch) = response {
            events.extend(batch);
        }
    }
    // Release before assertions so a failed assertion cannot strand the thread.
    finish.send(()).unwrap();
    let _host = worker.join().unwrap();
    assert!(matches!(
        &events[..],
        [Event::Accepted { id: 1 }, Event::Progress { id: 1, .. }]
    ));
    assert!(
        matches!(output.pop_front(), Some(Response::Events(events)) if matches!(&events[..], [Event::Completed { id: 1, .. }]))
    );
}

#[test]
fn failed_reconciliation_stays_blocked_until_explicit_retry() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = attempts.clone();
    let mut host = fixture().with_controller(
        "counter",
        Box::new(move |_, _| {
            if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Err(snap_store::Error::Unavailable)
            } else {
                Ok(())
            }
        }),
    );
    let (peer, _) = connect(&mut host, "alice", "blocked", 0);
    submit(&mut host, peer, 1, intent(1, 1));
    host.step();
    let output = host.drain(peer).unwrap();
    assert!(output.iter().any(|r| matches!(r, Response::Events(events) if events.iter().any(|event| matches!(event, Event::Completed { outcome: Err(snap_transport::Error::Application(value)), .. } if value["committed"] == true)))));
    host.recover_controllers().unwrap();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let state = host
        .transact("inspect failure", |tx| {
            Document::new(registry(), access()).lifecycle(tx, ID)
        })
        .unwrap();
    assert!(state.blocked.is_some());
    let mut retry = intent(2, 0);
    retry.mutation = "document.retry".into();
    retry.args = serde_json::Value::Null;
    submit(&mut host, peer, 2, retry);
    host.step();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    let state = host
        .transact("inspect cleared", |tx| {
            Document::new(registry(), access()).lifecycle(tx, ID)
        })
        .unwrap();
    assert!(state.blocked.is_none());
}

#[test]
fn carrier_close_during_controller_io_drains_only_accepted_work() {
    let (entered, waiting) = std::sync::mpsc::channel();
    let (finish, released) = std::sync::mpsc::channel();
    let mut host = fixture().with_controller(
        "counter",
        Box::new(move |_, _| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        }),
    );
    let (peer, _) = connect(&mut host, "alice", "closing-io", 0);
    let output = host.output(peer).unwrap();
    let control = host.carrier_control(peer).unwrap();
    submit(&mut host, peer, 1, intent(1, 1));
    submit(&mut host, peer, 2, intent(2, 10));
    let worker = std::thread::spawn(move || {
        host.step();
        host
    });
    waiting
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    control.close(1);
    control.detach(1); // Physical teardown must not downgrade desired close.
    finish.send(()).unwrap();
    let mut host = worker.join().unwrap();
    assert!(!host.step());
    assert!(host.retired(peer));
    assert_eq!(host.residency_references(ID), 0);
    let events: Vec<_> = std::iter::from_fn(|| output.pop_front())
        .filter_map(|r| match r {
            Response::Events(events) => Some(events),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Accepted { id: 1 }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Accepted { id: 2 }))
    );
    let saved = host
        .transact("read after close", |tx| {
            Document::new(registry(), access()).read(tx, ID, Some("alice"))
        })
        .unwrap();
    assert_eq!(saved.value, json!(1));
}

#[test]
fn residency_references_survive_socket_loss_and_close_only_after_drain() {
    let mut host = fixture();
    let (alice, _) = connect(&mut host, "alice", "a", 0);
    let (bob, _) = connect(&mut host, "bob", "b", 0);
    assert_eq!(host.residency_references(ID), 2);
    host.lost(alice, 1);
    assert_eq!(host.residency_references(ID), 2);
    submit(&mut host, bob, 1, intent(1, 1));
    host.submit(bob, Command::Close, 2).unwrap();
    assert_eq!(host.residency_references(ID), 2);
    host.step();
    assert_eq!(host.residency_references(ID), 1);
    host.tick(101);
    assert_eq!(host.residency_references(ID), 0);
}

#[test]
fn two_real_sdks_rebase_optimism_over_host_replication_and_recover_a_lost_result() {
    use snap_document::{
        ClientMessage,
        client::{Client, Outcome},
        wire::Wire,
    };
    fn receive(
        host: &mut Host<snap_sqlite::Sqlite>,
        peer: u64,
        wire: &mut Wire,
        client: &mut Client,
    ) {
        for response in host.drain(peer).unwrap() {
            for message in wire.receive(response).unwrap() {
                let outcome = client.handle(&registry(), message).unwrap();
                assert!(
                    !matches!(outcome, Outcome::NeedManifest { .. }),
                    "{outcome:?}"
                );
            }
        }
    }
    let mut host = fixture();
    let (alice, _) = connect(&mut host, "alice", "sdk-a", 0);
    let (bob, _) = connect(&mut host, "bob", "sdk-b", 0);
    let (mut a, mut b) = (Client::new("alice".into()), Client::new("bob".into()));
    let (mut aw, mut bw) = (Wire::default(), Wire::default());
    for (peer, wire, client) in [(alice, &mut aw, &mut a), (bob, &mut bw, &mut b)] {
        host.submit(
            peer,
            wire.submit(ClientMessage::Manifest(client.manifest()))
                .unwrap(),
            0,
        )
        .unwrap();
        host.step();
        receive(&mut host, peer, wire, client);
    }
    receive(&mut host, alice, &mut aw, &mut a);
    a.enqueue(&registry(), ID, "add", json!(5)).unwrap();
    a.enqueue(&registry(), ID, "add", json!(2)).unwrap();
    b.enqueue(&registry(), ID, "add", json!(10)).unwrap();
    assert_eq!(a.get(ID).unwrap().value, json!(7));
    assert_eq!(b.get(ID).unwrap().value, json!(10));
    host.submit(alice, aw.submit(a.next_submission().unwrap()).unwrap(), 0)
        .unwrap();
    receive(&mut host, alice, &mut aw, &mut a);
    host.submit(bob, bw.submit(b.next_submission().unwrap()).unwrap(), 0)
        .unwrap();
    receive(&mut host, bob, &mut bw, &mut b);
    host.submit(alice, aw.submit(a.next_submission().unwrap()).unwrap(), 0)
        .unwrap();
    receive(&mut host, alice, &mut aw, &mut a);
    assert_eq!(a.pending().len(), 2); // ACKs did not complete either write.
    for _ in 0..3 {
        assert!(host.step());
        receive(&mut host, alice, &mut aw, &mut a);
        receive(&mut host, bob, &mut bw, &mut b);
    }
    assert_eq!(a.get(ID).unwrap().value, json!(17));
    assert_eq!(a.authoritative(), b.authoritative());
    assert!(a.pending().is_empty() && b.pending().is_empty());
    a.enqueue(&registry(), ID, "add", json!(3)).unwrap();
    host.submit(alice, aw.submit(a.next_submission().unwrap()).unwrap(), 0)
        .unwrap();
    receive(&mut host, alice, &mut aw, &mut a);
    host.step();
    host.lost(alice, 10); // Drop the committed completion at the carrier boundary.
    a.begin_reconnect();
    let (replacement, resumed) = connect(&mut host, "alice", "sdk-a", 20);
    assert!(resumed);
    aw.reconnect();
    host.submit(
        replacement,
        aw.submit(ClientMessage::Manifest(a.manifest())).unwrap(),
        20,
    )
    .unwrap();
    host.step();
    receive(&mut host, replacement, &mut aw, &mut a);
    receive(&mut host, bob, &mut bw, &mut b);
    assert_eq!(a.get(ID).unwrap().value, json!(20));
    assert_eq!(a.authoritative(), b.authoritative());
    assert!(a.pending().is_empty());
}
