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
            Response::Notification { input, .. } => vec![serde_json::from_value(input).unwrap()],
            _ => vec![],
        })
        .collect()
}

#[test]
fn ack_allows_another_submission_before_first_completion_and_dispatch_stays_serial() {
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
    assert!(
        matches!(host.drain(alice).unwrap().as_slice(), [Response::Events(events)] if events == &vec![Event::Accepted{id:3}])
    );
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
fn revocation_filters_already_queued_intents_and_denies_accepted_writes() {
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
            .any(|m| matches!(m,ServerMessage::Completed(c) if c.result.is_err()))
    );
    let state = messages(manifest(&mut host, alice, 3, vec![]));
    assert!(
        state.iter().any(
            |m| matches!(m,ServerMessage::Manifest(state) if state.documents[0].value==json!(3))
        )
    );
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
    let recovery = messages(manifest(&mut host, replacement, 1, vec![intent(1, 7)]));
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
async fn websocket_accepts_a_second_command_while_first_completion_is_held() {
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
    // Both submissions crossed a real physical connection with no handler run.
    assert!(shared.host.lock().unwrap().step());
    assert!(shared.host.lock().unwrap().step());
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
fn physical_loss_keeps_owned_work_but_logical_expiry_fences_it() {
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
            1,
            if expire { vec![] } else { vec![intent(1, 5)] },
        ));
        assert!(
            recovered
                .iter()
                .any(|message| matches!(message, ServerMessage::Manifest(state)
            if state.documents[0].value == json!(if expire { 0 } else { 5 })
            && state.completed.len() == usize::from(!expire)))
        );
    }
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
    let mut aw = Wire::default();
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
