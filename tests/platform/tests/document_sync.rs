//! Document extent, replication and recovery through generic host assembly.
use snap_access::{
    Access, Actor, Audience, ChangeSet, GrantChange, KindDefinition, Resource, Role,
};
type Host<B> = snap_host::Blocking<B, snap_host::Application<B>>;
use snap_document::{
    Definition, Intent, Manifest, Mutation, Registry, ServerMessage, Snapshot, server::Document,
};
use snap_transport::operation::{Definition as Request, Guard};
use snap_transport::{Command, Event, Invocation, Response, json, server::Config};
use snap_transport_native::{Dispatcher, Shared};
use std::sync::Arc;

const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";

thread_local! {
    static ADMISSION: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[test]
fn declared_guards_short_circuit_before_acceptance_and_never_repeat_in_execution() {
    for rejected in [None, Some(0), Some(1), Some(2)] {
        ADMISSION.with(|trace| trace.borrow_mut().clear());
        let mut host = fixture_with(snap_transport::operation::Registry::default().with_request(
            Request {
                name: "fixture.guarded".into(),
                identity_required: true,
                input: |v| v.is_object(),
                output: |v| v.is_i64(),
                progress: |_| false,
                inputs: &[],
                error: |_| true,
                guards: vec![
                    Guard::policy(|_, _, v, _| guard_visit(0, v)),
                    Guard::policy(|_, _, v, _| guard_visit(1, v)),
                    Guard::policy(|_, _, v, _| guard_visit(2, v)),
                ],
                data: snap_store::Data::new(&[]),
                handler: snap_transport::operation::Handler::new(|tx, _, _, _, _| {
                    let doc = Document::new(registry());
                    let before = doc.retained(tx, ID)?;
                    doc.observe(tx, ID, json!(before.value.as_i64().unwrap() + 1))?;
                    Ok(json!(1))
                }),
            },
        ));
        let (peer, _) = connect(&mut host, "alice", "guards", 0);
        host.submit(
            peer,
            Command::Invoke(Invocation {
                id: 1,
                operation: "fixture.guarded".into(),
                input: json!({"reject": rejected}),
            }),
            0,
        )
        .unwrap();
        let admission = host.drain(peer).unwrap();
        if let Some(index) = rejected {
            assert!(matches!(
                admission.as_slice(),
                [Response::Event(Event::Completed {
                    outcome: Err(_),
                    ..
                })]
            ));
            assert!(!host.step());
            assert_eq!(
                ADMISSION.with(|trace| trace.borrow().clone()),
                (0..=index).collect::<Vec<_>>()
            );
        } else {
            assert_eq!(admission, vec![Response::Event(Event::Accepted { id: 1 })]);
            assert!(host.step());
            assert!(host.drain(peer).unwrap().iter().any(|response|
                matches!(response, Response::Event(Event::Completed { outcome: Ok(value), .. })
                    if value == &json!(1))));
            assert_eq!(
                ADMISSION.with(|trace| trace.borrow().clone()),
                vec![0, 1, 2]
            );
        }
        let committed = host
            .transact("inspect counter", |tx| {
                Document::new(registry()).retained(tx, ID)
            })
            .unwrap();
        assert_eq!(committed.value, json!(i64::from(rejected.is_none())));
    }
}

fn guard_visit(index: usize, input: &serde_json::Value) -> Result<(), snap_store::Error> {
    ADMISSION.with(|trace| trace.borrow_mut().push(index));
    if input["reject"].as_u64() == Some(index as u64) {
        Err(snap_store::Error::Invalid)
    } else {
        Ok(())
    }
}

#[test]
fn invalid_declared_output_rolls_back_and_releases_the_next_operation() {
    for failure in ["output", "cold", "invalid"] {
        let mut host = fixture_with(snap_transport::operation::Registry::default().with_request(
            Request {
                name: "fixture.invalid-output".into(),
                identity_required: true,
                input: |v| matches!(v.as_str(), Some("output" | "cold" | "invalid")),
                output: |v| v.is_i64(),
                progress: |_| false,
                inputs: &[],
                error: |_| true,
                guards: vec![],
                data: snap_store::Data::new(&[]),
                handler: snap_transport::operation::Handler::new(|tx, call, _, _, _| {
                    Document::new(registry()).observe(tx, ID, json!(99))?;
                    if call.input == "cold" {
                        // A caught Store failure still poisons the whole transaction.
                        assert!(matches!(
                            tx.find(snap_document::server::TABLES[0], "primary", &[]),
                            Err(snap_store::Error::Miss(_))
                        ));
                    } else if call.input == "invalid" {
                        assert_eq!(
                            tx.get(snap_document::server::TABLES[0], &[]),
                            Err(snap_store::Error::Invalid)
                        );
                    }
                    Ok(json!("not the declared output"))
                }),
            },
        ));
        let (peer, _) = connect(&mut host, "alice", "invalid-output", 0);
        host.submit(
            peer,
            Command::Invoke(Invocation {
                id: 1,
                operation: "fixture.invalid-output".into(),
                input: json!(failure),
            }),
            0,
        )
        .unwrap();
        submit(&mut host, peer, 2, intent(1, 1));
        assert_eq!(
            host.drain(peer).unwrap(),
            vec![Response::Event(Event::Accepted { id: 1 })]
        );
        assert!(host.step());
        let terminal: Vec<_> = host
            .drain(peer)
            .unwrap()
            .into_iter()
            .filter(|response| matches!(response, Response::Event(_)))
            .collect();
        assert_eq!(
            terminal,
            vec![Response::Event(Event::Completed {
                id: 1,
                outcome: Err(match failure {
                    "cold" => snap_transport::Error::Application(json!({"code":"StoreMiss"})),
                    "invalid" => snap_transport::Error::Application(json!({"code":"Rejected"})),
                    _ => snap_transport::Error::InvalidOutput,
                }),
            })]
        );
        assert!(host.step());
        let committed = host
            .transact("inspect rollback", |tx| {
                Document::new(registry()).retained(tx, ID)
            })
            .unwrap();
        assert_eq!(committed.value, json!(1));
    }
}

#[test]
fn accepted_table_residency_survives_connection_housekeeping_without_readmission() {
    for housekeeping in ["none", "connect", "close", "expire"] {
        let mut host = fixture_with(snap_transport::operation::Registry::default().with_request(
            Request {
                name: "fixture.scan".into(),
                identity_required: true,
                input: |v| v.is_null(),
                output: |v| v.is_u64(),
                progress: |_| false,
                inputs: &[],
                error: |_| true,
                guards: vec![Guard::policy(|tx, _, _, _| {
                    tx.find(snap_document::server::TABLES[0], "primary", &[])?;
                    Ok(())
                })],
                data: snap_store::Data::new(&[snap_document::server::TABLES[0]]),
                handler: snap_transport::operation::Handler::new(|tx, _, _, _, _| {
                    Ok(json!(
                        tx.find(snap_document::server::TABLES[0], "primary", &[])?
                            .len()
                    ))
                }),
            },
        ));
        let (peer, _) = connect(&mut host, "alice", "scan", 0);
        let other = if matches!(housekeeping, "close" | "expire") {
            Some(connect(&mut host, "bob", "other", 0).0)
        } else {
            None
        };
        if housekeeping == "expire" {
            host.submit(other.unwrap(), Command::Disconnect, 0).unwrap();
        }
        host.submit(
            peer,
            Command::Invoke(Invocation {
                id: 1,
                operation: "fixture.scan".into(),
                input: json!(null),
            }),
            0,
        )
        .unwrap();
        assert_eq!(
            host.drain(peer).unwrap(),
            vec![Response::Event(Event::Accepted { id: 1 })]
        );
        match housekeeping {
            "connect" => {
                connect(&mut host, "bob", "other", 0);
            }
            "close" => {
                host.submit(other.unwrap(), Command::Close, 0).unwrap();
            }
            "expire" => host.tick(100),
            _ => {}
        }
        assert!(host.step());
        let terminal: Vec<_> = host
            .drain(peer)
            .unwrap()
            .into_iter()
            .filter(|response| matches!(response, Response::Event(_)))
            .collect();
        assert_eq!(
            terminal,
            vec![Response::Event(Event::Completed {
                id: 1,
                outcome: Ok(json!(1))
            })],
            "{housekeeping}"
        );
    }
}

#[test]
fn http_operations_share_fifo_and_cannot_run_on_connected_carriers() {
    let mut host = fixture_with(
        snap_transport::operation::Registry::default().with_preconnection_request(Request {
            name: "fixture.fetch".into(),
            identity_required: false,
            input: |value| value.is_null(),
            output: |value| value.is_i64(),
            progress: |_| false,
            inputs: &[],
            error: |_| true,
            guards: vec![],
            data: snap_store::Data::new(&[snap_document::server::TABLES[0]]),
            handler: snap_transport::operation::Handler::new(|tx, _, _, _, _| {
                Ok(Document::new(registry()).read(tx, ID, Some("alice"))?.value)
            }),
        }),
    );
    let (peer, _) = connect(&mut host, "alice", "http-fifo", 0);
    submit(&mut host, peer, 1, intent(1, 7));
    let invocation = Invocation {
        id: 1,
        operation: "fixture.fetch".into(),
        input: json!(null),
    };
    assert_eq!(
        host.submit(peer, Command::Invoke(invocation.clone()), 0),
        Err(snap_transport::Error::UnknownOperation)
    );
    assert_eq!(
        host.submit(
            peer,
            Command::Request {
                bearer: None,
                invocation: invocation.clone()
            },
            0
        ),
        Err(snap_transport::Error::UnknownOperation)
    );
    // The prior mutation is accepted but not executed. HTTP must enter its FIFO,
    // then read the committed result. Repeated calls also prove peers are freed.
    for _ in 0..140 {
        assert_eq!(
            host.preconnection_request(invocation.clone(), None),
            Ok(json!(7))
        );
    }
    let invalid = Invocation {
        input: json!({"unexpected":true}),
        ..invocation
    };
    assert_eq!(
        host.preconnection_request(invalid, None),
        Err(snap_transport::Error::InvalidInput)
    );
}

#[test]
fn controller_dependencies_load_explicitly_and_finalizers_retain_hidden_values() {
    const DEP: &str = "018f3c4b-6d2a-7000-8000-000000000002";
    for cleanup in [false, true] {
        let mut host = fixture();
        host.transact("dependency", |tx| {
            let doc = Document::new(registry());
            doc.create(
                tx,
                &Snapshot {
                    id: DEP.into(),
                    kind: "counter".into(),
                    version: "1".into(),
                    revision: 1,
                    value: json!(12),
                },
                Audience::Restricted,
                "bob",
            )?;
            if cleanup {
                doc.remove(tx, DEP, "bob")?;
                let resource = snap_document::server::resource(DEP);
                let mut lifecycle = resource.lifecycle(tx)?;
                lifecycle.finalizers.insert("resource".into());
                resource.set_lifecycle(tx, &lifecycle)?;
            }
            Ok(())
        })
        .unwrap();
        let (peer, _) = connect(&mut host, "alice", "dependency", 0);
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = called.clone();
        let mut host = host.map_participant(|documents| {
            documents.with_controller(document_controller(
                "counter",
                Box::new(move |ctx, snapshot| {
                    if snapshot.id != ID {
                        return Ok(());
                    }
                    let resident = ctx.inspect("resident dependency", |tx| {
                        Document::new(registry()).retained(tx, DEP)
                    });
                    if cleanup {
                        assert_eq!(resident?.value, json!(12));
                    } else {
                        assert!(matches!(resident, Err(snap_store::Error::Miss(_))));
                    }
                    ctx.row(&snap_document::server::resource(DEP))?;
                    assert_eq!(
                        ctx.inspect("dependency", |tx| Document::new(registry())
                            .retained(tx, DEP))?
                            .value,
                        json!(12)
                    );
                    if cleanup {
                        ctx.finalize(&snap_document::server::resource(DEP), "resource")?;
                    }
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }),
            ))
        });
        submit(&mut host, peer, 1, intent(1, 1));
        assert!(host.step());
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }
}

fn access() -> Access {
    Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap()
}

fn document_controller(
    kind: &str,
    mut run: Box<
        dyn FnMut(
                &mut snap_host::ControllerContext<'_, '_, snap_store_sqlite::Sqlite>,
                Snapshot,
            ) -> Result<(), snap_store::Error>
            + Send,
    >,
) -> snap_host::Controller<snap_store_sqlite::Sqlite> {
    let selected = kind.to_owned();
    snap_host::Controller::new(
        kind,
        snap_document::server::TABLES[0],
        move |row| row.get("kind") == Some(&selected.clone().into()),
        move |ctx, resource| {
            let [snap_store::Value::Text(id)] = resource.key.as_slice() else {
                return Err(snap_store::Error::Invalid);
            };
            let snapshot = ctx.inspect("document.controller", |tx| {
                Document::new(registry()).retained(tx, id)
            })?;
            run(ctx, snapshot)
        },
    )
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

fn fixture() -> Host<snap_store_sqlite::Sqlite> {
    fixture_with(snap_transport::operation::Registry::default())
}

fn fixture_with(
    mut operations: snap_transport::operation::Registry,
) -> Host<snap_store_sqlite::Sqlite> {
    let mut migrations: Vec<snap_store::migration::Migration> = vec![
        toml::from_str(snap_access::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::MIGRATION).unwrap(),
        toml::from_str(snap_store::resource::MIGRATION).unwrap(),
    ];
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
    {
        store.load(table).unwrap();
    }
    let document = Arc::new(Document::new(registry()));
    for definition in snap_document::operations::definitions(document.clone()) {
        operations = operations.with_request(definition);
    }
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
        snap_host::Application::new(vec![snap_document::sync::binding(document)]),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |_, bearer| match bearer {
                "alice" | "bob" => Ok(bearer.into()),
                _ => Err(snap_store::Error::NotFound),
            },
        ))),
        Config {
            reconnect_ms: 100,
            capacity: 16,
        },
        "fixture-boot".into(),
    )
}

fn connect(
    host: &mut Host<snap_store_sqlite::Sqlite>,
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
    host: &mut Host<snap_store_sqlite::Sqlite>,
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
fn submit(host: &mut Host<snap_store_sqlite::Sqlite>, peer: u64, wire: u64, intent: Intent) {
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
            Response::Event(Event::Completed {
                outcome: Ok(value), ..
            }) => vec![serde_json::from_value(value).unwrap()],
            Response::Global { input, .. } => match serde_json::from_value(input).unwrap() {
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
        matches!(host.drain(alice).unwrap().as_slice(), [Response::Event(event)] if matches!(event, Event::Accepted { id: 2 }))
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
    // A new wire invocation recovers the same Document receipt, independently
    // of any Transport invocation history.
    submit(&mut host, replacement, 4, intent(1, 7));
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
async fn cookie_required_and_mixed_agent_carriers_keep_distinct_authority_policies() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Error, Message, client::IntoClientRequest},
    };
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    for (required, cookie, explicit, allowed) in [
        (false, None, "alice", true),
        (false, Some("invalid"), "alice", true),
        (false, Some("alice"), "", true),
        (true, None, "alice", false),
        (true, Some("invalid"), "alice", false),
        (true, Some("alice"), "invalid", true),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let read: snap_transport_ws::ReadCookie = Arc::new(|headers| {
            headers
                .get("cookie")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        });
        let shared = Shared::new(fixture());
        let router = snap_transport_ws::router(Arc::new(snap_transport_ws::Service {
            dispatch: Dispatcher::web(shared),
            origin: format!("http://{address}"),
            cookie: Some(read),
            require_cookie: required,
        }));
        let _stop = Stop(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        let mut request = format!("ws://{address}/transport")
            .into_client_request()
            .unwrap();
        if let Some(cookie) = cookie {
            request
                .headers_mut()
                .insert("cookie", cookie.parse().unwrap());
        }
        let connection =
            tokio::time::timeout(std::time::Duration::from_secs(2), connect_async(request))
                .await
                .unwrap();
        if !allowed {
            match connection {
                Err(Error::Http(response)) => assert_eq!(response.status(), 401),
                other => panic!("expected rejected upgrade: {other:?}"),
            }
            continue;
        }
        let (mut socket, _) = connection.unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&Command::Connect {
                    bearer: explicit.into(),
                    client_id: "policy-fixture".into(),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(response.to_text().unwrap()).unwrap(),
            Response::Attached { resumed: false }
        ));
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "real socket suite"]
async fn websocket_delivers_ack_before_execution_and_serializes_following_acceptance() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shared = Shared::new(fixture());
    let router = snap_transport_ws::router(Arc::new(snap_transport_ws::Service {
        dispatch: Dispatcher::web(shared.clone()),
        origin: format!("http://{address}"),
        cookie: None,
        require_cookie: false,
    }));
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
            Response::Event(Event::Accepted { id })
        );
    }
    // The first ACK crossed the socket before any handler ran. Drive the first
    // completion; the second command may still be waiting in the carrier reader.
    assert!(shared.host.lock().unwrap().step());
    let _dispatch = Stop(tokio::spawn(snap_transport_native::dispatch(
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
    let host = fixture().map_participant(|documents| {
        documents.with_controller(document_controller(
            "counter",
            Box::new(move |_, _| {
                entered.take().unwrap().send(()).unwrap();
                released
                    .recv()
                    .map_err(|_| snap_store::Error::Unavailable)?;
                Ok(())
            }),
        ))
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shared = Shared::new(host);
    let router = snap_transport_ws::router(Arc::new(snap_transport_ws::Service {
        dispatch: Dispatcher::web(shared.clone()),
        origin: format!("http://{address}"),
        cookie: None,
        require_cookie: false,
    }));
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
        Response::Event(Event::Accepted { id: 1 })
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
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        0
    );
    assert_eq!(
        host.transact("read drained mutation", |tx| Document::new(registry())
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
    let mut host = fixture().map_participant(|documents| {
        documents.with_controller(document_controller(
            "counter",
            Box::new(move |ctx, snapshot| {
                log.lock().unwrap().push(snapshot.value.clone());
                if snapshot.value == json!(1) {
                    ctx.progress(json!({"message":"preparing"}))?;
                    ctx.transact("controller.observe", |tx| {
                        Document::new(registry()).replace(tx, ID, "alice", json!(2))?;
                        Ok(())
                    })?;
                }
                Ok(())
            }),
        ))
    });
    let (peer, _) = connect(&mut host, "alice", "controller", 0);
    submit(&mut host, peer, 1, intent(1, 1));
    submit(&mut host, peer, 2, intent(2, 10));
    assert_eq!(
        host.drain(peer).unwrap(),
        vec![Response::Event(Event::Accepted { id: 1 })]
    );
    host.step();
    assert_eq!(*observations.lock().unwrap(), vec![json!(1), json!(2)]);
    let events: Vec<_> = host
        .drain(peer)
        .unwrap()
        .into_iter()
        .flat_map(|response| match response {
            Response::Event(event) => vec![event],
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
    let mut host = fixture().map_participant(|documents| {
        documents.with_controller(document_controller(
            "counter",
            Box::new(move |ctx, _| {
                ctx.progress(json!("waiting for IO"))?;
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            }),
        ))
    });
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
        if let Response::Event(event) = response {
            let batch = vec![event];
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
    assert!(matches!(
        output.pop_front(),
        Some(Response::Event(Event::Completed { id: 1, .. }))
    ));
}

#[test]
fn failed_reconciliation_stays_blocked_until_explicit_retry() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = attempts.clone();
    let mut host = fixture().map_participant(|documents| {
        documents.with_controller(document_controller(
            "counter",
            Box::new(move |_, _| {
                if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Err(snap_store::Error::Unavailable)
                } else {
                    Ok(())
                }
            }),
        ))
    });
    let (peer, _) = connect(&mut host, "alice", "blocked", 0);
    submit(&mut host, peer, 1, intent(1, 1));
    host.step();
    let output = host.drain(peer).unwrap();
    assert!(output.iter().any(|r| matches!(r, Response::Event(Event::Completed { outcome: Err(snap_transport::Error::Application(value)), .. }) if value["committed"] == true)));
    host.recover().unwrap();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let state = host
        .transact("inspect failure", |tx| {
            snap_document::server::resource(ID).lifecycle(tx)
        })
        .unwrap();
    assert!(state.blocked.is_some());
    host.transact("explicit resource retry", |tx| {
        snap_document::server::resource(ID).retry(tx)
    })
    .unwrap();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    let state = host
        .transact("inspect cleared", |tx| {
            snap_document::server::resource(ID).lifecycle(tx)
        })
        .unwrap();
    assert!(state.blocked.is_none());
}

#[test]
fn carrier_close_during_controller_io_drains_only_accepted_work() {
    let (entered, waiting) = std::sync::mpsc::channel();
    let (finish, released) = std::sync::mpsc::channel();
    let mut host = fixture().map_participant(|documents| {
        documents.with_controller(document_controller(
            "counter",
            Box::new(move |_, _| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            }),
        ))
    });
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
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        0
    );
    let events: Vec<_> = std::iter::from_fn(|| output.pop_front())
        .filter_map(|r| match r {
            Response::Event(event) => Some(vec![event]),
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
            Document::new(registry()).read(tx, ID, Some("alice"))
        })
        .unwrap();
    assert_eq!(saved.value, json!(1));
}

#[test]
fn residency_references_survive_socket_loss_and_close_only_after_drain() {
    let mut host = fixture();
    let (alice, _) = connect(&mut host, "alice", "a", 0);
    let (bob, _) = connect(&mut host, "bob", "b", 0);
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        2
    );
    host.lost(alice, 1);
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        2
    );
    submit(&mut host, bob, 1, intent(1, 1));
    host.submit(bob, Command::Close, 2).unwrap();
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        2
    );
    host.step();
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        1
    );
    host.tick(101);
    assert_eq!(
        host.residency_references(&snap_document::server::resource(ID)),
        0
    );
}

#[test]
fn reattaching_a_direct_peer_starts_with_fresh_document_holdings() {
    for detach in [Command::Disconnect, Command::Close] {
        let mut host = fixture();
        let (peer, _) = connect(&mut host, "alice", "old-attachment", 0);
        let initial = messages(manifest(&mut host, peer, 1, vec![]));
        assert!(
            initial
                .iter()
                .any(|m| matches!(m, ServerMessage::Holdings(d) if d.len() == 1 && d[0].id == ID))
        );
        host.submit(peer, detach, 1).unwrap();
        assert!(host.drain(peer).unwrap().contains(&Response::Detached));
        host.submit(
            peer,
            Command::Connect {
                bearer: "bob".into(),
                client_id: "new-attachment".into(),
            },
            2,
        )
        .unwrap();
        assert_eq!(
            host.drain(peer).unwrap(),
            [Response::Attached { resumed: false }]
        );
        host.transact("synchronize unchanged state", |_| Ok(()))
            .unwrap();
        let replacement = messages(host.drain(peer).unwrap());
        assert!(
            replacement
                .iter()
                .any(|m| matches!(m, ServerMessage::Holdings(d) if d.len() == 1 && d[0].id == ID))
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
        host: &mut Host<snap_store_sqlite::Sqlite>,
        peer: u64,
        wire: &mut Wire,
        client: &mut Client,
    ) {
        for response in host.drain(peer).unwrap() {
            // One frame carries at most one message now, so there is no inner
            // loop. Acceptance and progress produce none.
            if let Some(message) = wire.receive(response).unwrap() {
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
    let (mut ai, mut bi) = (
        snap_transport::client::InvocationIds::default(),
        snap_transport::client::InvocationIds::default(),
    );
    for (peer, wire, client, ids) in [
        (alice, &mut aw, &mut a, &mut ai),
        (bob, &mut bw, &mut b, &mut bi),
    ] {
        host.submit(
            peer,
            wire.submit(ids, ClientMessage::Manifest(client.manifest()))
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
    host.submit(
        alice,
        aw.submit(&mut ai, a.next_submission().unwrap()).unwrap(),
        0,
    )
    .unwrap();
    receive(&mut host, alice, &mut aw, &mut a);
    host.submit(
        bob,
        bw.submit(&mut bi, b.next_submission().unwrap()).unwrap(),
        0,
    )
    .unwrap();
    receive(&mut host, bob, &mut bw, &mut b);
    host.submit(
        alice,
        aw.submit(&mut ai, a.next_submission().unwrap()).unwrap(),
        0,
    )
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
    host.submit(
        alice,
        aw.submit(&mut ai, a.next_submission().unwrap()).unwrap(),
        0,
    )
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
        aw.submit(&mut ai, ClientMessage::Manifest(a.manifest()))
            .unwrap(),
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

#[test]
fn connectionless_bearer_handoff_is_separate_from_output_and_requires_successful_commit() {
    use snap_transport::bearer::{Change, Receiver, Token};
    for failure in [0, 1, 2] {
        let mut host = fixture_with(
            snap_transport::operation::Registry::default().with_preconnection_request(Request {
                name: "fixture.issue".into(),
                identity_required: false,
                input: |v| v.is_null(),
                output: if failure == 2 {
                    |_| false
                } else {
                    |v| v.is_null()
                },
                progress: |_| false,
                error: |_| true,
                guards: vec![],
                inputs: &[],
                data: Document::new(registry()).data(),
                handler: snap_transport::operation::Handler::new(move |tx, _, _, _, context| {
                    Document::new(registry()).observe(tx, ID, json!(1))?;
                    context
                        .bearer_changed(Change::Set(Token::new("private-token".into())))
                        .unwrap();
                    if failure == 1 {
                        return Err(snap_store::Error::Unavailable);
                    }
                    Ok(json!(null))
                }),
            }),
        );
        let reply = host.preconnection_reply(
            Invocation {
                id: 7,
                operation: "fixture.issue".into(),
                input: json!(null),
            },
            None,
        );
        if failure == 0 {
            assert_eq!(reply.outcome, Ok(json!(null)));
            assert!(
                matches!(reply.bearer, Some(Change::Set(token)) if token.expose() == "private-token")
            );
        } else {
            assert!(reply.outcome.is_err());
            assert!(reply.bearer.is_none());
        }
        let snapshot = host
            .transact("committed", |tx| Document::new(registry()).retained(tx, ID))
            .unwrap();
        assert_eq!(snapshot.value, json!(if failure == 0 { 1 } else { 0 }));
    }
}

#[test]
fn connectionless_reply_preserves_admission_for_failed_execution() {
    let mut host = fixture_with(
        snap_transport::operation::Registry::default().with_preconnection_request(Request {
            name: "fixture.failure".into(),
            identity_required: false,
            input: |v| v.is_null(),
            output: |v| v.is_null(),
            progress: |_| false,
            error: |_| true,
            guards: vec![],
            inputs: &[],
            data: snap_store::Data::default(),
            handler: snap_transport::operation::Handler::new(|_, _, _, _, _| {
                Err(snap_store::Error::Unavailable)
            }),
        }),
    );
    for (input, accepted) in [(json!(null), true), (json!({}), false)] {
        let reply = host.preconnection_reply(
            Invocation {
                id: 1,
                operation: "fixture.failure".into(),
                input,
            },
            None,
        );
        assert_eq!(reply.accepted, accepted);
        assert!(reply.outcome.is_err());
        assert!(reply.bearer.is_none());
    }
}
