//! Store resource ownership and generic host controllers over non-Document rows.
#[path = "../support/host.rs"]
mod assembly;
use snap_host::{Application, Blocking, Controller};
use snap_platform_tests::{cartridge, memory::Memory};
use snap_store::{
    Catalog, Error, Row, Store,
    residency::Residency,
    resource::{self, Resource, State},
};
use snap_transport::{Command, Event, Invocation, Response, json};
use std::sync::{Arc, Mutex};

fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut migrations = assembly::migrations();
    migrations.push(toml::from_str(resource::MIGRATION).unwrap());
    migrations.push(
        toml::from_str(
            r#"
id = "9998_composite"
[[changes]]
action = "create_table"
[changes.table]
name = "other.items"
primary = ["owner", "id"]
columns = [{name="owner",kind="text"},{name="id",kind="integer"},{name="value",kind="text"}]
"#,
        )
        .unwrap(),
    );
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    migrations
}
#[test]
fn lifecycle_is_namespaced_transactional_and_retains_composite_cleanup_keys() {
    let migrations = migrations();
    let catalog = migrations
        .iter()
        .try_fold(Catalog::default(), |catalog, m| m.apply(&catalog))
        .unwrap();
    let memory = Store::new(catalog.clone(), Memory::new(catalog).unwrap()).unwrap();
    check_resources(memory);
    check_resources(snap_store_sqlite::Sqlite::memory(&migrations).unwrap());
}
fn check_resources<B: snap_store::Backend>(mut store: Store<B>) {
    resource::data().prepare(&mut store).unwrap();
    store.load("other.items").unwrap();
    let first = Resource::new("other.items", &["alice".into(), 1.into()]);
    let second = Resource::new("other.items", &["bob".into(), 1.into()]);
    store
        .run("create", |tx| {
            for owner in ["alice", "bob"] {
                tx.insert(
                    "other.items",
                    Row::from([
                        ("owner".into(), owner.into()),
                        ("id".into(), 1.into()),
                        ("value".into(), "payload".into()),
                    ]),
                )?;
            }
            let mut lifecycle = first.lifecycle(tx)?;
            lifecycle.state = State::Deleted;
            lifecycle.finalizers.insert("external.files".into());
            lifecycle.blocked = Some("cleanup failed".into());
            first.set_lifecycle(tx, &lifecycle)
        })
        .unwrap();
    assert!(matches!(
        store.run("aborted retry", |tx| {
            first.retry(tx)?;
            Err::<(), _>(Error::Unavailable)
        }),
        Err(Error::Unavailable)
    ));
    store
        .inspect("ownership", |tx| {
            assert_eq!(
                first.lifecycle(tx)?.blocked.as_deref(),
                Some("cleanup failed")
            );
            assert_eq!(second.lifecycle(tx)?.state, State::Active);
            assert!(second.lifecycle(tx)?.finalizers.is_empty());
            assert_eq!(
                resource::cleanup_keys(tx, "other.items")?,
                [first.key.clone()].into_iter().collect()
            );
            Ok(())
        })
        .unwrap();
    let mut residency = Residency::default();
    residency.set(
        "other.items",
        "viewer".into(),
        [second.key.clone()].into_iter().collect(),
    );
    residency.remove("other.items", "viewer");
    residency.apply(&mut store, &Default::default()).unwrap();
    assert!(
        store
            .inspect("retained cleanup", |tx| tx.get("other.items", &first.key))
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        store.inspect("unneeded reader", |tx| tx.get("other.items", &second.key)),
        Err(Error::Miss(_))
    ));
    store
        .run("finish cleanup", |tx| {
            first.retry(tx)?;
            first.finalize(tx, "external.files")
        })
        .unwrap();
    residency.apply(&mut store, &Default::default()).unwrap();
    assert!(matches!(
        store.inspect("released cleanup", |tx| tx.get("other.items", &first.key)),
        Err(Error::Miss(_))
    ));
    store
        .load_keys("other.items", &[first.key.clone()].into_iter().collect())
        .unwrap();
    assert!(
        store
            .inspect("durable row not purged", |tx| tx
                .get("other.items", &first.key))
            .unwrap()
            .is_some()
    );
}

#[test]
fn first_connectionless_commit_prepares_cold_controller_metadata_and_finishes_every_pass() {
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    store
        .run("seed", |tx| {
            for table in cartridge::TABLES {
                tx.insert(
                    table,
                    Row::from([("id".into(), 1.into()), ("value".into(), 0.into())]),
                )?;
            }
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        store.inspect("cold metadata", |tx| tx.find(
            resource::TABLE,
            "primary",
            &[]
        )),
        Err(Error::Miss(_))
    ));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = Application::new(vec![]);
    for table in cartridge::TABLES {
        let seen = log.clone();
        app = app.with_controller(Controller::new(
            table,
            table,
            |_| true,
            move |ctx, resource| {
                let row = ctx.row(&resource)?;
                seen.lock()
                    .unwrap()
                    .push((resource.table.clone(), row["value"].clone()));
                if row["value"] == 1.into() {
                    ctx.transact("reconcile", |tx| {
                        tx.update(
                            table,
                            &resource.key,
                            Row::from([("value".into(), 2.into())]),
                        )
                    })?;
                }
                Ok(())
            },
        ));
    }
    let operations = cartridge::definitions().into_iter().fold(
        snap_transport::operation::Registry::default(),
        |registry, definition| registry.with_preconnection_request(definition),
    );
    let mut host = Blocking::new(
        store,
        app,
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |_, bearer| Ok(bearer.into()),
        ))),
        Default::default(),
        "cold-controller".into(),
    );
    let outcome = host.preconnection_request(
        Invocation {
            id: 1,
            operation: "probe.change".into(),
            input: serde_json::to_value(cartridge::Edit {
                expected: 0,
                amount: 1,
                stop: cartridge::Stop::Commit,
            })
            .unwrap(),
        },
        Some("alice".into()),
    );
    assert_eq!(outcome, Ok(json!([1, 1])));
    assert_eq!(
        *log.lock().unwrap(),
        [
            ("probe.left".into(), 1.into()),
            ("probe.left".into(), 2.into()),
            ("probe.right".into(), 1.into()),
            ("probe.right".into(), 2.into()),
        ]
    );
    assert_eq!(
        host.preconnection_request(
            Invocation {
                id: 2,
                operation: "probe.read".into(),
                input: json!(null)
            },
            Some("alice".into())
        ),
        Ok(json!([2, 2]))
    );
}

#[test]
fn generic_controllers_drain_independent_resources_and_recover_only_after_explicit_retry() {
    let migrations = migrations();
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    resource::data().prepare(&mut store).unwrap();
    let seed = assembly::mount(store, Default::default(), "seed".into()).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = Application::new(vec![]);
    for table in cartridge::TABLES {
        let seen = log.clone();
        app = app.with_controller(Controller::new(
            table,
            table,
            |_| true,
            move |ctx, resource| {
                let row = ctx.row(&resource)?;
                seen.lock()
                    .unwrap()
                    .push((resource.table.clone(), row["value"].clone()));
                if table == cartridge::TABLES[0] && row["value"] == 1.into() {
                    return Err(Error::Unavailable);
                }
                if row["value"] == 1.into() {
                    ctx.transact("observed", |tx| {
                        tx.update(
                            table,
                            &resource.key,
                            Row::from([("value".into(), 2.into())]),
                        )
                    })?;
                }
                ctx.progress(json!({"table":table,"value":row["value"]}))
            },
        ));
    }
    let mut host = seed.map_participant(|()| app);
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: "alice".into(),
            client_id: "resources".into(),
        },
        0,
    )
    .unwrap();
    host.drain(peer).unwrap();
    host.submit(
        peer,
        Command::Invoke(Invocation {
            id: 1,
            operation: "probe.change".into(),
            input: serde_json::to_value(cartridge::Edit {
                expected: 0,
                amount: 1,
                stop: cartridge::Stop::Commit,
            })
            .unwrap(),
        }),
        0,
    )
    .unwrap();
    assert_eq!(
        host.drain(peer).unwrap(),
        [Response::Event(Event::Accepted { id: 1 })]
    );
    assert!(host.step());
    let outputs = host.drain(peer).unwrap();
    assert!(
        matches!(outputs.last(),Some(Response::Event(Event::Completed{outcome:Err(snap_transport::Error::Application(v)),..})) if v["committed"]==true)
    );
    assert_eq!(
        *log.lock().unwrap(),
        [
            ("probe.left".into(), 1.into()),
            ("probe.right".into(), 1.into()),
            ("probe.right".into(), 2.into())
        ]
    );
    host.recover().unwrap();
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(table, _)| table == "probe.left")
            .count(),
        1
    );
    host.transact("explicit retry with new desired state", |tx| {
        let resource = Resource::new("probe.left", &[1.into()]);
        resource.retry(tx)?;
        tx.update(
            "probe.left",
            &resource.key,
            Row::from([("value".into(), 3.into())]),
        )
    })
    .unwrap();
    assert_eq!(
        log.lock().unwrap().last(),
        Some(&("probe.left".into(), 3.into()))
    );
    assert!(host.drain(peer).unwrap().is_empty());
    assert!(!host.step());
}

fn subscription(
    topic: &str,
    table: &'static str,
    actor: &'static str,
) -> snap_transport::subscription::Definition {
    use snap_transport::{Value, subscription::Definition};
    Definition {
        topic: topic.into(),
        table,
        data: resource::data(),
        extent: Box::new(move |_, who| {
            Ok(if who == actor {
                [vec![1.into()]].into_iter().collect()
            } else {
                Default::default()
            })
        }),
        read: Box::new(move |tx, _, who| {
            if who != actor {
                return Ok(Value::Null);
            }
            let row = tx.get(table, &[1.into()])?.ok_or(Error::NotFound)?;
            Ok(json!({"value":row["value"]}))
        }),
        changes: Box::new(|before, after, _, _| {
            Ok(if before != after {
                vec![after.clone()]
            } else {
                vec![]
            })
        }),
        origin: Box::new(|_| Ok(None)),
        filter: Box::new(|input, extent| {
            if !extent.contains(&vec![1.into()]) {
                *input = Value::Null;
            }
            Ok(true)
        }),
        expire: Box::new(|_, _| Ok(())),
        reset: Value::Null,
    }
}

#[test]
fn independent_subscription_extents_redact_backlogs_without_losing_accepted_completions() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    resource::data().prepare(&mut store).unwrap();
    store
        .run("seed", |tx| {
            for table in cartridge::TABLES {
                tx.insert(
                    table,
                    Row::from([("id".into(), 1.into()), ("value".into(), 0.into())]),
                )?;
            }
            Ok(())
        })
        .unwrap();
    let valid = Arc::new(AtomicBool::new(true));
    let live = valid.clone();
    let authority = snap_transport::bearer::Callbacks::with_retained(
        Arc::new(move |_, bearer| {
            if bearer == "alice" && !live.load(Ordering::SeqCst) {
                Err(Error::NotFound)
            } else {
                Ok(bearer.into())
            }
        }),
        Arc::new(|_, bearer| Ok(bearer.into())),
    );
    let operations = cartridge::definitions().into_iter().fold(
        snap_transport::operation::Registry::default(),
        |registry, definition| registry.with_request(definition),
    );
    let mut host = Blocking::new(
        store,
        Application::new(vec![
            subscription("left", cartridge::TABLES[0], "alice"),
            subscription("right", cartridge::TABLES[1], "bob"),
        ]),
        operations,
        Arc::new(authority),
        Default::default(),
        "subscriptions".into(),
    );
    let mut peers = Vec::new();
    for actor in ["alice", "bob"] {
        let peer = host.open().unwrap();
        host.submit(
            peer,
            Command::Connect {
                bearer: actor.into(),
                client_id: actor.into(),
            },
            0,
        )
        .unwrap();
        assert_eq!(
            host.drain(peer).unwrap(),
            [Response::Attached { resumed: false }]
        );
        peers.push(peer);
    }
    host.transact("initial publications", |_| Ok(())).unwrap();
    let alice = host.output(peers[0]).unwrap();
    let bob = host.output(peers[1]).unwrap();
    assert!(
        matches!(alice.front(),Some(Response::Global{kind,input}) if kind=="left"&&input==json!({"value":0}))
    );
    assert!(
        matches!(bob.front(),Some(Response::Global{kind,input}) if kind=="right"&&input==json!({"value":0}))
    );
    host.submit(
        peers[0],
        Command::Invoke(Invocation {
            id: 1,
            operation: "probe.change".into(),
            input: serde_json::to_value(cartridge::Edit {
                expected: 0,
                amount: 1,
                stop: cartridge::Stop::Commit,
            })
            .unwrap(),
        }),
        0,
    )
    .unwrap();
    valid.store(false, Ordering::SeqCst);
    assert!(host.step());
    let mut events = Vec::new();
    // Production independent Output, not the direct-host delivery filter.
    while let Some(response) = alice.pop_front() {
        match response {
            Response::Global { input, .. } => {
                assert!(input.is_null(), "expired contents escaped: {input}")
            }
            Response::Event(event) => events.push(event),
            other => panic!("unexpected output: {other:?}"),
        }
    }
    assert_eq!(
        events,
        [
            Event::Accepted { id: 1 },
            Event::Completed {
                id: 1,
                outcome: Ok(json!([1, 1]))
            }
        ]
    );
    let mut values = Vec::new();
    while let Some(response) = bob.pop_front() {
        if let Response::Global { kind, input } = response {
            assert_eq!(kind, "right");
            values.push(input);
        }
    }
    assert_eq!(values, [json!({"value":0}), json!({"value":1})]);
    host.submit(peers[0], Command::Disconnect, 1).unwrap();
    host.drain(peers[0]).unwrap();
    host.submit(
        peers[0],
        Command::Connect {
            bearer: "bob".into(),
            client_id: "bob-replacement".into(),
        },
        2,
    )
    .unwrap();
    host.drain(peers[0]).unwrap();
    host.transact("new physical observer", |_| Ok(())).unwrap();
    assert!(
        matches!(alice.pop_front(),Some(Response::Global{kind,input}) if kind=="right"&&input==json!({"value":1}))
    );
}
