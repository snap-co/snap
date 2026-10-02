use snap_store::{Kind, Row, Value};
use snap_store::{migration::*, *};
use snap_store_sqlite::{Sqlite, migrate};

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
