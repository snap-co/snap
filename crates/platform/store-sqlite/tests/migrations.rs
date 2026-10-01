use snap_store::{Kind, Row};
use snap_store::{migration::*, *};
use snap_store_sqlite::{Sqlite, migrate, status};

fn path(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "snap-migrations-{}-{name}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}
fn initial() -> Migration {
    Migration {
        id: "0001_people".into(),
        changes: vec![Change::CreateTable {
            table: Table {
                name: "identity.people".into(),
                columns: vec![
                    Column {
                        name: "id".into(),
                        kind: Kind::Integer,
                    },
                    Column {
                        name: "name".into(),
                        kind: Kind::Text,
                    },
                ],
                primary: vec!["id".into()],
                indexes: vec![],
                foreign: vec![],
            },
        }],
    }
}

#[test]
fn explicit_ddl_preserves_rows_and_records_immutable_history() {
    let path = path("evolve");
    let mut migrations = vec![initial()];
    migrate(&path, &migrations).unwrap();
    let mut store = Sqlite::open(&path).unwrap();
    store
        .run("create", |tx| {
            tx.insert(
                "identity.people",
                Row::from([("id".into(), 1.into()), ("name".into(), "Alice".into())]),
            )
        })
        .unwrap();
    drop(store);
    migrations.push(Migration {
        id: "0002_contact".into(),
        changes: vec![
            Change::AddColumn {
                table: "identity.people".into(),
                column: Column {
                    name: "active".into(),
                    kind: Kind::Integer,
                },
                fill: 1.into(),
            },
            Change::RenameColumn {
                table: "identity.people".into(),
                from: "name".into(),
                to: "display".into(),
            },
            Change::CreateIndex {
                table: "identity.people".into(),
                index: Index {
                    name: "display".into(),
                    columns: vec!["display".into()],
                    unique: true,
                },
            },
        ],
    });
    assert_eq!(
        status(&path, &migrations).unwrap().pending,
        vec!["0002_contact"]
    );
    assert_eq!(
        migrate(&path, &migrations).unwrap().applied,
        vec!["0002_contact"]
    );
    assert!(migrate(&path, &migrations).unwrap().applied.is_empty());
    let mut store = Sqlite::open(&path).unwrap();
    store.load("identity.people").unwrap();
    let row = store
        .run("read", |tx| tx.get("identity.people", &[1.into()]))
        .unwrap()
        .value
        .unwrap();
    assert_eq!(
        row,
        Row::from([
            ("id".into(), 1.into()),
            ("display".into(), "Alice".into()),
            ("active".into(), 1.into())
        ])
    );
    drop(store);
    let mut edited = migrations.clone();
    edited[0].id = "0000_different".into();
    assert!(migrate(&path, &edited).is_err());
    assert!(migrate(&path, &migrations[..1]).is_err());
    migrations.push(Migration {
        id: "0003_remove".into(),
        changes: vec![
            Change::DropIndex {
                table: "identity.people".into(),
                index: "display".into(),
            },
            Change::DropTable {
                table: "identity.people".into(),
            },
        ],
    });
    migrate(&path, &migrations).unwrap();
    assert!(Sqlite::open(&path).unwrap().catalog().tables.is_empty());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn failed_ddl_rolls_back_data_shape_and_migration_journal_together() {
    let path = path("rollback");
    let first = initial();
    migrate(&path, std::slice::from_ref(&first)).unwrap();
    let mut store = Sqlite::open(&path).unwrap();
    store
        .run("duplicates", |tx| {
            for id in [1, 2] {
                tx.insert(
                    "identity.people",
                    Row::from([("id".into(), id.into()), ("name".into(), "same".into())]),
                )?;
            }
            Ok(())
        })
        .unwrap();
    drop(store);
    let bad = Migration {
        id: "0002_unique".into(),
        changes: vec![
            Change::AddColumn {
                table: "identity.people".into(),
                column: Column {
                    name: "extra".into(),
                    kind: Kind::Bytes,
                },
                fill: snap_store::Value::Bytes(vec![]),
            },
            Change::CreateIndex {
                table: "identity.people".into(),
                index: Index {
                    name: "name".into(),
                    columns: vec!["name".into()],
                    unique: true,
                },
            },
        ],
    };
    assert!(migrate(&path, &[first.clone(), bad]).is_err());
    assert_eq!(
        status(&path, &[first]).unwrap().applied,
        vec!["0001_people"]
    );
    let mut store = Sqlite::open(&path).unwrap();
    assert_eq!(store.catalog().tables[0].columns.len(), 2);
    store.load("identity.people").unwrap();
    assert_eq!(
        store
            .run("read", |tx| tx.find("identity.people", "primary", &[]))
            .unwrap()
            .value
            .len(),
        2
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn out_of_band_ddl_is_rejected_instead_of_silently_adopted() {
    let path = path("drift");
    migrate(&path, &[initial()]).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("ALTER TABLE \"identity.people\" ADD COLUMN surprise TEXT")
        .unwrap();
    drop(connection);
    assert!(Sqlite::open(&path).is_err());
    assert!(migrate(&path, &[initial()]).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn indexes_cannot_refer_to_columns_added_later_in_the_migration() {
    let path = path("index-order");
    let first = initial();
    migrate(&path, std::slice::from_ref(&first)).unwrap();
    let mut store = Sqlite::open(&path).unwrap();
    store
        .run("create", |tx| {
            tx.insert(
                "identity.people",
                Row::from([("id".into(), 1.into()), ("name".into(), "Alice".into())]),
            )
        })
        .unwrap();
    drop(store);
    let mut second = Migration {
        id: "0002_email".into(),
        changes: vec![
            Change::CreateIndex {
                table: "identity.people".into(),
                index: Index {
                    name: "email".into(),
                    columns: vec!["email".into()],
                    unique: true,
                },
            },
            Change::AddColumn {
                table: "identity.people".into(),
                column: Column {
                    name: "email".into(),
                    kind: Kind::Text,
                },
                fill: "alice".into(),
            },
        ],
    };
    assert!(migrate(&path, &[first.clone(), second.clone()]).is_err());
    assert_eq!(
        status(&path, std::slice::from_ref(&first)).unwrap().applied,
        vec!["0001_people"]
    );
    let mut store = Sqlite::open(&path).unwrap();
    store.load("identity.people").unwrap();
    assert_eq!(
        store
            .run("unchanged", |tx| tx.get("identity.people", &[1.into()]))
            .unwrap()
            .value
            .unwrap()
            .len(),
        2
    );
    drop(store);

    second.changes.reverse();
    migrate(&path, &[first, second]).unwrap();
    let mut store = Sqlite::open(&path).unwrap();
    let duplicate = store.run("duplicate", |tx| {
        tx.insert(
            "identity.people",
            Row::from([
                ("id".into(), 2.into()),
                ("name".into(), "Bob".into()),
                ("email".into(), "alice".into()),
            ]),
        )
    });
    assert!(matches!(duplicate, Err(Error::Constraint)));
    store.load("identity.people").unwrap();
    assert_eq!(
        store
            .run("one", |tx| tx.find(
                "identity.people",
                "email",
                &["alice".into()]
            ))
            .unwrap()
            .value
            .len(),
        1
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}
