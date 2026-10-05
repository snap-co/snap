use snap_store::{
    migration::{Change, Migration},
    *,
};
use snap_store_sqlite::{Sqlite, migrate};

fn migrations() -> Vec<Migration> {
    vec![Migration {
        id: "0001_invites".into(),
        changes: vec![
            Change::CreateTable {
                table: Table {
                    name: "app.invites".into(),
                    columns: vec![
                        Column {
                            name: "id".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "recipient".into(),
                            kind: Kind::Text,
                        },
                    ],
                    primary: vec!["id".into()],
                    indexes: vec![Index {
                        name: "recipient".into(),
                        columns: vec!["recipient".into()],
                        unique: true,
                    }],
                    foreign: vec![],
                },
            },
            Change::CreateTable {
                table: Table {
                    name: "app.outbox".into(),
                    columns: vec![
                        Column {
                            name: "invite".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "body".into(),
                            kind: Kind::Bytes,
                        },
                    ],
                    primary: vec!["invite".into()],
                    indexes: vec![],
                    foreign: vec![ForeignKey {
                        columns: vec!["invite".into()],
                        table: "app.invites".into(),
                        references: vec!["id".into()],
                    }],
                },
            },
        ],
    }]
}

fn path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "snap-program-{}-{label}.sqlite",
        std::process::id()
    ))
}

fn invite(id: i64, recipient: &str) -> Row {
    Row::from([
        ("id".into(), id.into()),
        ("recipient".into(), recipient.into()),
    ])
}

#[test]
fn durable_program_log_rebuilds_from_a_checkpoint_and_rolls_back_with_data() {
    let source = path("source");
    let checkpoint = path("checkpoint");
    for path in [&source, &checkpoint] {
        assert!(!path.exists());
    }
    migrate(&source, &migrations()).unwrap();
    let mut store = Sqlite::open(&source).unwrap();
    store
        .run("checkpoint state", |tx| {
            tx.insert("app.invites", invite(1, "alice"))
        })
        .unwrap();
    let checkpoint_position = store.programs(0, 1).unwrap()[0].position;
    assert_eq!(checkpoint_position, 1);
    drop(store);
    // The connection is closed and SQLite's rollback journal is settled. This
    // file copy is a fixed checkpoint, not a concurrent backup mechanism.
    std::fs::copy(&source, &checkpoint).unwrap();

    let mut store = Sqlite::open(&source).unwrap();
    for table in ["app.invites", "app.outbox"] {
        store.load(table).unwrap();
    }
    let authored = store
        .run("send invite", |tx| {
            tx.insert(
                "app.outbox",
                Row::from([
                    ("invite".into(), 2.into()),
                    ("body".into(), Value::Bytes(vec![0, 255])),
                ]),
            )?;
            tx.insert("app.invites", invite(2, "bob"))?;
            tx.update(
                "app.invites",
                &[1.into()],
                Row::from([("recipient".into(), "carol".into())]),
            )?;
            tx.insert("app.invites", invite(3, "temporary"))?;
            tx.delete("app.invites", &[3.into()])?;
            Ok(())
        })
        .unwrap()
        .program;
    // This deferred FK fails at COMMIT, after the adapter inserted the log row.
    let failed = store.run("invalid invite", |tx| {
        tx.insert("app.invites", invite(4, "dave"))?;
        tx.insert(
            "app.outbox",
            Row::from([
                ("invite".into(), 99.into()),
                ("body".into(), Value::Bytes(vec![1])),
            ]),
        )
    });
    assert!(matches!(failed, Err(Error::Constraint)));
    assert!(matches!(
        store.run("handler rejects", |tx| {
            tx.insert("app.invites", invite(5, "eve"))?;
            Err::<(), _>(Error::Invalid)
        }),
        Err(Error::Invalid)
    ));
    store
        .inspect("read only", |tx| tx.get("app.invites", &[1.into()]))
        .unwrap();
    drop(store);

    let mut reopened = Sqlite::open(&source).unwrap();
    let tail = reopened.programs(checkpoint_position, 1).unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].position, 2);
    assert_eq!(tail[0].program.as_bytes(), authored.as_bytes());
    assert!(reopened.programs(tail[0].position, 1).unwrap().is_empty());
    assert_eq!(reopened.programs(0, 10).unwrap().len(), 2);
    drop(reopened);
    drop(authored);

    let mut restored = Sqlite::open(&checkpoint).unwrap();
    for table in ["app.invites", "app.outbox"] {
        restored.load(table).unwrap();
    }
    for entry in tail {
        restored.replay(&entry.program).unwrap();
    }
    drop(restored);
    let mut restored = Sqlite::open(&checkpoint).unwrap();
    for table in ["app.invites", "app.outbox"] {
        restored.load(table).unwrap();
    }
    assert_eq!(
        restored
            .inspect("invites", |tx| tx.find("app.invites", "primary", &[]))
            .unwrap(),
        vec![invite(1, "carol"), invite(2, "bob")]
    );
    assert_eq!(
        restored
            .inspect("outbox", |tx| tx.find("app.outbox", "primary", &[]))
            .unwrap(),
        vec![Row::from([
            ("invite".into(), 2.into()),
            ("body".into(), Value::Bytes(vec![0, 255]))
        ])]
    );
    assert_eq!(
        restored
            .inspect("index", |tx| tx.find(
                "app.invites",
                "recipient",
                &["carol".into()]
            ))
            .unwrap(),
        vec![invite(1, "carol")]
    );
    drop(restored);
    for path in [source, checkpoint] {
        std::fs::remove_file(path).unwrap();
    }
}
