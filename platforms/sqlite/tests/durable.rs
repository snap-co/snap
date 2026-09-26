use snap_sqlite::{Sqlite, migrate};
use snap_store::resident::{migration::*, *};
use snap_store::{Kind, Row, Value};

fn migrations() -> Vec<Migration> {
    vec![Migration {
        id: "0001_accounts".into(),
        changes: vec![Change::CreateTable {
            table: Table {
                name: "identity.accounts".into(),
                columns: vec![
                    Column {
                        name: "id".into(),
                        kind: Kind::Integer,
                    },
                    Column {
                        name: "email".into(),
                        kind: Kind::Text,
                    },
                ],
                primary: vec!["id".into()],
                indexes: vec![Index {
                    name: "email".into(),
                    columns: vec!["email".into()],
                    unique: true,
                }],
                foreign: vec![],
            },
        }],
    }]
}
fn row(id: i64, email: &str) -> Row {
    Row::from([("id".into(), id.into()), ("email".into(), email.into())])
}

#[test]
fn committed_rows_are_immediate_hits_and_survive_reopen() {
    let path =
        std::env::temp_dir().join(format!("snap-store-durable-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    migrate(&path, &migrations()).unwrap();
    let mut store = Sqlite::open(&path).unwrap();
    store
        .run("create", |tx| {
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    assert_eq!(
        store
            .run("get", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .value,
        Some(row(1, "alice"))
    );
    // Another owner cannot serve stale data or migrate under this instance.
    assert!(Sqlite::open(&path).is_err());
    assert!(migrate(&path, &migrations()).is_err());
    drop(store);
    let mut store = Sqlite::open(&path).unwrap();
    assert!(matches!(
        store.run("cold", |tx| tx.get("identity.accounts", &[1.into()])),
        Err(Error::Miss(_))
    ));
    store.load("identity.accounts").unwrap();
    assert_eq!(
        store
            .run("email", |tx| tx.find(
                "identity.accounts",
                "email",
                &[Value::from("alice")]
            ))
            .unwrap()
            .value,
        vec![row(1, "alice")]
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn secondary_indexes_and_negative_results_change_with_the_commit() {
    let mut store = Sqlite::memory(&migrations()).unwrap();
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .run("nx", |tx| tx.find(
                "identity.accounts",
                "email",
                &["alice".into()]
            ))
            .unwrap()
            .value
            .is_empty()
    );
    store
        .run("insert", |tx| {
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    store
        .run("update", |tx| {
            tx.update(
                "identity.accounts",
                &[1.into()],
                Row::from([("email".into(), "bob".into())]),
            )?;
            assert!(
                tx.find("identity.accounts", "email", &["alice".into()])?
                    .is_empty()
            );
            assert_eq!(
                tx.find("identity.accounts", "email", &["bob".into()])?,
                vec![row(1, "bob")]
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .run("bob", |tx| tx.find(
                "identity.accounts",
                "email",
                &["bob".into()]
            ))
            .unwrap()
            .value,
        vec![row(1, "bob")]
    );
    store
        .run("delete", |tx| tx.delete("identity.accounts", &[1.into()]))
        .unwrap();
    assert!(
        store
            .run("deleted", |tx| tx.find(
                "identity.accounts",
                "email",
                &["bob".into()]
            ))
            .unwrap()
            .value
            .is_empty()
    );
}

#[test]
fn cross_module_constraints_roll_back_every_write_including_memory() {
    let mut migrations = migrations();
    migrations[0].changes.push(Change::CreateTable {
        table: Table {
            name: "access.grants".into(),
            columns: vec![Column {
                name: "account".into(),
                kind: Kind::Integer,
            }],
            primary: vec!["account".into()],
            indexes: vec![],
            foreign: vec![ForeignKey {
                columns: vec!["account".into()],
                table: "identity.accounts".into(),
                references: vec!["id".into()],
            }],
        },
    });
    let mut store = Sqlite::memory(&migrations).unwrap();
    for table in ["identity.accounts", "access.grants"] {
        store.load(table).unwrap();
    }
    let result = store.run("invalid signup", |tx| {
        tx.insert("identity.accounts", row(1, "alice"))?;
        tx.insert("access.grants", Row::from([("account".into(), 2.into())]))
    });
    assert!(matches!(result, Err(Error::Constraint)));
    assert!(
        store
            .run("memory", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .value
            .is_none()
    );
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .run("disk", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .value
            .is_none()
    );
    // FK checks happen on the final transaction, permitting module write order.
    store
        .run("valid signup", |tx| {
            tx.insert("access.grants", Row::from([("account".into(), 1.into())]))?;
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    assert!(matches!(
        store.run("delete referenced", |tx| tx
            .delete("identity.accounts", &[1.into()])),
        Err(Error::Constraint)
    ));
    assert!(
        store
            .run("still present", |tx| tx
                .get("identity.accounts", &[1.into()]))
            .unwrap()
            .value
            .is_some()
    );
}

#[test]
fn late_miss_discards_cross_module_writes_and_returns_without_loading() {
    let mut migrations = migrations();
    migrations[0].changes.push(Change::CreateTable {
        table: Table {
            name: "access.policy".into(),
            columns: vec![Column {
                name: "id".into(),
                kind: Kind::Integer,
            }],
            primary: vec!["id".into()],
            indexes: vec![],
            foreign: vec![],
        },
    });
    let mut store = Sqlite::memory(&migrations).unwrap();
    store.load("identity.accounts").unwrap();
    let mut attempts = 0;
    let result = store.run("signup", |tx| {
        attempts += 1;
        assert!(tx.get("identity.accounts", &[1.into()])?.is_none());
        tx.insert("identity.accounts", row(1, "alice"))?;
        tx.get("access.policy", &[0.into()])?;
        Ok(())
    });
    assert!(matches!(result, Err(Error::Miss(_))));
    assert_eq!(attempts, 1);
    assert!(
        store
            .run("memory", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .value
            .is_none()
    );
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .run("disk", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .value
            .is_none()
    );
    assert!(matches!(
        store.run("still cold", |tx| tx.get("access.policy", &[0.into()])),
        Err(Error::Miss(_))
    ));
    store.load("access.policy").unwrap();
    assert!(
        store
            .run("known nx", |tx| tx.get("access.policy", &[0.into()]))
            .unwrap()
            .value
            .is_none()
    );
}

#[test]
fn unique_index_failure_discards_earlier_statements_and_allows_the_next_operation() {
    let mut store = Sqlite::memory(&migrations()).unwrap();
    store.load("identity.accounts").unwrap();
    store
        .run("alice", |tx| {
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    let failed = store.run("duplicate email", |tx| {
        tx.insert("identity.accounts", row(2, "bob"))?;
        tx.insert("identity.accounts", row(3, "alice"))
    });
    assert!(matches!(failed, Err(Error::Constraint)));
    assert_eq!(
        store
            .run("resident", |tx| tx.find(
                "identity.accounts",
                "primary",
                &[]
            ))
            .unwrap()
            .value,
        vec![row(1, "alice")]
    );
    store.load("identity.accounts").unwrap();
    assert_eq!(
        store
            .run("durable", |tx| tx.find("identity.accounts", "primary", &[]))
            .unwrap()
            .value,
        vec![row(1, "alice")]
    );
    store
        .run("bob", |tx| tx.insert("identity.accounts", row(2, "bob")))
        .unwrap();
}

#[test]
fn compound_keys_preserve_integer_byte_and_text_values_through_sqlite() {
    let migration = Migration {
        id: "0001_typed".into(),
        changes: vec![Change::CreateTable {
            table: Table {
                name: "typed.records".into(),
                columns: vec![
                    Column {
                        name: "bytes".into(),
                        kind: Kind::Bytes,
                    },
                    Column {
                        name: "number".into(),
                        kind: Kind::Integer,
                    },
                    Column {
                        name: "text".into(),
                        kind: Kind::Text,
                    },
                ],
                primary: vec!["bytes".into(), "number".into()],
                indexes: vec![Index {
                    name: "text".into(),
                    columns: vec!["text".into()],
                    unique: false,
                }],
                foreign: vec![],
            },
        }],
    };
    let mut store = Sqlite::memory(&[migration]).unwrap();
    let a = Row::from([
        ("bytes".into(), Value::Bytes(vec![0])),
        ("number".into(), i64::MIN.into()),
        ("text".into(), "a\0é".into()),
    ]);
    let b = Row::from([
        ("bytes".into(), Value::Bytes(vec![255])),
        ("number".into(), i64::MAX.into()),
        ("text".into(), "".into()),
    ]);
    store
        .run("write", |tx| {
            tx.insert("typed.records", b.clone())?;
            tx.insert("typed.records", a.clone())
        })
        .unwrap();
    store.load("typed.records").unwrap();
    assert_eq!(
        store
            .run("primary", |tx| tx.find("typed.records", "primary", &[]))
            .unwrap()
            .value,
        vec![a.clone(), b.clone()]
    );
    assert_eq!(
        store
            .run("secondary", |tx| tx.find("typed.records", "text", &[]))
            .unwrap()
            .value,
        vec![b, a.clone()]
    );
    assert_eq!(
        store
            .run("lookup", |tx| tx.get(
                "typed.records",
                &[Value::Bytes(vec![0]), i64::MIN.into()]
            ))
            .unwrap()
            .value,
        Some(a)
    );
}
