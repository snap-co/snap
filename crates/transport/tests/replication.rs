//! Non-Document replication protects the host's automatic operation/controller
//! program handoff. Composite keys and bytes are deliberately not profile-shaped.
use snap_store::{
    memory::Memory,
    replica::{Publication, Replica},
    *,
};
use snap_transport::{
    host::{Blocking, Controller, Controllers},
    replication::{Declaration, Registry, Replications},
    *,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Change;
impl Operation for Change {
    const NAME: &'static str = "work.change";
    type Input = bool;
    type Output = ();
    type Error = ();
    type Progress = ();
}
fn table() -> Table {
    Table {
        name: "work.items".into(),
        columns: vec![
            Column {
                name: "owner".into(),
                kind: Kind::Text,
            },
            Column {
                name: "id".into(),
                kind: Kind::Integer,
            },
            Column {
                name: "amount".into(),
                kind: Kind::Integer,
            },
            Column {
                name: "status".into(),
                kind: Kind::Text,
            },
            Column {
                name: "payload".into(),
                kind: Kind::Bytes,
            },
        ],
        primary: vec!["owner".into(), "id".into()],
        indexes: vec![],
        foreign: vec![],
    }
}
#[test]
fn committed_operations_and_controller_programs_replicate_in_order_but_failed_attempts_do_not() {
    let mut catalog = Catalog::new(vec![table()]).unwrap();
    let migration: migration::Migration = toml::from_str(resource::MIGRATION).unwrap();
    catalog = migration.apply(&catalog).unwrap();
    let mut store = Store::new(catalog.clone(), Memory::new(catalog).unwrap()).unwrap();
    store.load("work.items").unwrap();
    store.load(resource::TABLE).unwrap();
    store
        .run("seed", |tx| {
            tx.insert(
                "work.items",
                Row::from([
                    ("owner".into(), "alice".into()),
                    ("id".into(), 7.into()),
                    ("amount".into(), 0.into()),
                    ("status".into(), "done".into()),
                    ("payload".into(), snap_store::Value::Bytes(vec![0, 255])),
                ]),
            )
        })
        .unwrap();
    let policy_available = Arc::new(AtomicBool::new(true));
    let policy = policy_available.clone();
    let replication = Arc::new(
        Registry::new(vec![Declaration::new(
            table(),
            Data::new(&["work.items"]),
            move |_, actor, key| {
                if !policy.load(Ordering::SeqCst) {
                    return Err(snap_store::Error::Unavailable);
                }
                Ok(key.first() == Some(&actor.into()))
            },
        )])
        .unwrap(),
    );
    let registry = operation::Registry::default()
        .with_request(replication.operation())
        .with_request(operation::Definition::typed::<Change>(
            true,
            vec![],
            Data::new(&["work.items"]),
            &[],
            |tx, reject, _| {
                tx.update(
                    "work.items",
                    &["alice".into(), 7.into()],
                    Row::from([
                        ("amount".into(), 10.into()),
                        ("status".into(), "pending".into()),
                    ]),
                )?;
                if reject {
                    return Err(operation::TypedFailure::Application(()));
                }
                Ok(())
            },
        ));
    let participant = Controllers::around(Replications::new(replication.clone())).with_controller(
        Controller::new(
            "settle",
            "work.items",
            |row| row.get("status") == Some(&"pending".into()),
            |ctx, resource| {
                ctx.transact("settle", |tx| {
                    tx.update(
                        &resource.table,
                        &resource.key,
                        Row::from([
                            ("status".into(), "done".into()),
                            ("payload".into(), snap_store::Value::Bytes(vec![42])),
                        ]),
                    )
                })
            },
        ),
    );
    let authority = Arc::new(bearer::Callbacks::new(Arc::new(|_, token| {
        if token == "alice-token" {
            Ok("alice".into())
        } else {
            Err(snap_store::Error::NotFound)
        }
    })));
    let mut host = Blocking::new(
        store,
        participant,
        registry,
        authority,
        server::Config::default(),
        "replication".into(),
    );
    host.recover().unwrap();
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: "alice-token".into(),
            client_id: "one".into(),
        },
        0,
    )
    .unwrap();
    host.drain(peer).unwrap();
    let manifest = replication::Subscription {
        tables: [(
            "work.items".into(),
            [vec!["alice".into(), 7.into()]].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
    };
    let mut replica = Replica::new(replication.catalog().clone()).unwrap();
    let submit = |host: &mut Blocking<_, _>, id, operation: &str, input| {
        host.submit(
            peer,
            Command::Invoke(Invocation {
                id,
                operation: operation.into(),
                input,
            }),
            0,
        )
        .unwrap();
        while host.step() {}
        host.drain(peer).unwrap()
    };
    for response in submit(&mut host, 1, "store.subscribe", json!(manifest)) {
        if let Response::Global { input, .. } = response {
            replica
                .apply(&serde_json::from_value::<Publication>(input).unwrap())
                .unwrap();
        }
    }
    let failed = submit(&mut host, 2, Change::NAME, json!(true));
    assert!(failed.iter().any(|r| matches!(
        r,
        Response::Event(Event::Completed {
            outcome: Err(_),
            ..
        })
    )));
    assert!(failed.iter().all(|r| !matches!(r, Response::Global { .. })));
    let frames = submit(&mut host, 3, Change::NAME, json!(false));
    let mut programs = Vec::new();
    for response in &frames {
        if let Response::Global { kind, input } = response {
            assert_eq!(kind, replication::TOPIC);
            let publication: Publication = serde_json::from_value(input.clone()).unwrap();
            assert!(!publication.reset);
            programs
                .push(Program::from_bytes(replication.catalog(), &publication.program).unwrap());
            replica.apply(&publication).unwrap();
        }
    }
    assert_eq!(programs.len(), 2);
    assert_eq!(
        programs[0].instructions().collect::<Vec<_>>(),
        vec![Instruction::Update {
            table: "work.items".into(),
            key: vec!["alice".into(), 7.into()],
            changes: Row::from([
                ("amount".into(), 10.into()),
                ("status".into(), "pending".into())
            ]),
        }]
    );
    assert_eq!(
        programs[1].instructions().collect::<Vec<_>>(),
        vec![Instruction::Update {
            table: "work.items".into(),
            key: vec!["alice".into(), 7.into()],
            changes: Row::from([
                ("status".into(), "done".into()),
                ("payload".into(), snap_store::Value::Bytes(vec![42]))
            ]),
        }]
    );
    assert_eq!(
        replica
            .get("work.items", &["alice".into(), 7.into()])
            .unwrap(),
        Some(Row::from([
            ("owner".into(), "alice".into()),
            ("id".into(), 7.into()),
            ("amount".into(), 10.into()),
            ("status".into(), "done".into()),
            ("payload".into(), snap_store::Value::Bytes(vec![42])),
        ]))
    );
    // Policy failure must also redact the independent carrier queue, not just
    // make direct host drain return an error while old bytes remain available.
    host.submit(
        peer,
        Command::Invoke(Invocation {
            id: 4,
            operation: Change::NAME.into(),
            input: json!(false),
        }),
        0,
    )
    .unwrap();
    while host.step() {}
    policy_available.store(false, Ordering::SeqCst);
    assert_eq!(
        host.transact("reauthorize", |_| Ok(())),
        Err(snap_store::Error::Unavailable)
    );
    let output = host.output(peer).unwrap();
    let mut redacted = 0;
    while let Some(response) = output.pop_front() {
        if let Response::Global { input, .. } = response {
            let publication: Publication = serde_json::from_value(input).unwrap();
            assert!(publication.reset);
            assert!(
                Program::from_bytes(replication.catalog(), &publication.program)
                    .unwrap()
                    .is_empty()
            );
            replica.apply(&publication).unwrap();
            redacted += 1;
        }
    }
    assert_eq!(redacted, 1);
    assert!(
        replica
            .get("work.items", &["alice".into(), 7.into()])
            .unwrap()
            .is_none()
    );
}
