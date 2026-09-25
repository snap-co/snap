//! Store is an application-facing local capability. These promises (cross-table
//! snapshots, constraints and transactions) cannot be expressed by Identity's SDK.
use snap_store::{
    Cache, Error, ForeignKey, Guard, Index, Kind, Predicate as P, Query, Row, Rows, Schema,
    Statement as S, Store, Table, Transaction, Value,
};

const ROOT: Table = Table {
    namespace: "documents",
    name: "records",
};
const EXTENT: Table = Table {
    namespace: "extents",
    name: "records",
};
const BINARY: Table = Table {
    namespace: "binary",
    name: "records",
};
pub fn schemas() -> [Schema; 3] {
    [
        Schema {
            table: ROOT,
            columns: &[
                ("id", Kind::Text),
                ("locator", Kind::Text),
                ("revision", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[Index {
                columns: &["locator"],
                unique: true,
            }],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: EXTENT,
            columns: &[
                ("id", Kind::Text),
                ("root", Kind::Text),
                ("revision", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[Index {
                columns: &["root"],
                unique: false,
            }],
            foreign: &[ForeignKey {
                columns: &["root"],
                target: ROOT,
                references: &["id"],
            }],
            legacy_name: None,
        },
        Schema {
            table: BINARY,
            columns: &[("id", Kind::Integer), ("data", Kind::Bytes)],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
    ]
}
fn root(id: &str, locator: &str, revision: i64) -> Row {
    [
        ("id".into(), id.into()),
        ("locator".into(), locator.into()),
        ("revision".into(), revision.into()),
    ]
    .into()
}
fn extent(revision: i64) -> Row {
    [
        ("id".into(), "e".into()),
        ("root".into(), "r".into()),
        ("revision".into(), revision.into()),
    ]
    .into()
}
fn tx(statements: Vec<S>) -> Transaction {
    Transaction {
        guards: vec![],
        statements,
    }
}
async fn read(store: &impl Store, table: Table) -> Rows {
    store
        .transaction(tx(vec![S::Select(Query::new(table))]))
        .await
        .unwrap()
        .remove(0)
}
fn change(table: Table, revision: i64) -> S {
    S::Update {
        table,
        filter: vec![],
        changes: [("revision".into(), revision.into())].into(),
    }
}

pub async fn contract(store: impl Store, peer: impl Store, cache: impl Cache) {
    store
        .transaction(tx(vec![
            S::Insert {
                table: ROOT,
                row: root("r", "unique", 1),
            },
            S::Insert {
                table: EXTENT,
                row: extent(1),
            },
        ]))
        .await
        .unwrap();
    assert_eq!(read(&store, ROOT).await, vec![root("r", "unique", 1)]);
    assert_eq!(read(&store, EXTENT).await, vec![extent(1)]);
    // Unique and referential failures roll back earlier writes in the same batch.
    assert_eq!(
        store
            .transaction(tx(vec![
                change(EXTENT, 2),
                S::Insert {
                    table: ROOT,
                    row: root("duplicate", "unique", 2)
                }
            ]))
            .await,
        Err(Error::Constraint)
    );
    assert_eq!(read(&store, EXTENT).await, vec![extent(1)]);
    assert_eq!(
        store
            .transaction(tx(vec![S::Delete {
                table: ROOT,
                filter: vec![]
            }]))
            .await,
        Err(Error::Constraint)
    );
    assert_eq!(
        store
            .transaction(tx(vec![S::Insert {
                table: ROOT,
                row: [("id".into(), Value::Integer(7))].into()
            }]))
            .await,
        Err(Error::Invalid)
    );
    assert_eq!(read(&store, ROOT).await, vec![root("r", "unique", 1)]);
    // A stale optimistic guard cannot partially advance a document frontier.
    assert_eq!(
        store
            .transaction(Transaction {
                guards: vec![Guard {
                    query: Query::new(ROOT).matching(vec![P::eq("revision", 0_i64)]),
                    exists: true
                }],
                statements: vec![change(ROOT, 2), change(EXTENT, 2)]
            })
            .await,
        Err(Error::Conflict)
    );
    let concurrent_claim = || {
        tx(vec![S::Insert {
            table: ROOT,
            row: root("claim", "claimed", 0),
        }])
    };
    let (a, b) = futures_util::join!(
        store.transaction(concurrent_claim()),
        peer.transaction(concurrent_claim())
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(Error::Constraint)) || matches!(b, Err(Error::Constraint)));
    store
        .transaction(tx(vec![S::Delete {
            table: ROOT,
            filter: vec![P::eq("id", "claim")],
        }]))
        .await
        .unwrap();
    // Distinct SQLite connections must still give a coherent cross-namespace read.
    let writer = async {
        for revision in 2..=24 {
            peer.transaction(tx(vec![change(ROOT, revision), change(EXTENT, revision)]))
                .await
                .unwrap();
        }
    };
    let reader = async {
        for _ in 0..24 {
            let results = store
                .transaction(tx(vec![
                    S::Select(Query::new(ROOT)),
                    S::Select(Query::new(EXTENT)),
                ]))
                .await
                .unwrap();
            assert_eq!(results[0][0]["revision"], results[1][0]["revision"]);
        }
    };
    futures_util::join!(writer, reader);
    assert_eq!(read(&store, ROOT).await[0]["revision"], Value::Integer(24));
    let query = Query::new(ROOT)
        .matching(vec![P::eq("locator", "unique")])
        .limit(1);
    let snapshot = snap_store::snapshot(&store, &cache, query.clone())
        .await
        .unwrap();
    assert_eq!(cache.get(&query), Some(snapshot.clone()));
    peer.transaction(tx(vec![change(ROOT, 25), change(EXTENT, 25)]))
        .await
        .unwrap();
    assert_eq!(
        snap_store::snapshot(&store, &cache, query.clone())
            .await
            .unwrap(),
        snapshot
    );
    assert_eq!(
        snap_store::snapshot(&store, &snap_store::NoCache, query)
            .await
            .unwrap()[0]["revision"],
        Value::Integer(25)
    );
    assert_eq!(read(&store, ROOT).await[0]["revision"], Value::Integer(25));
    // Full signed integers and binary payloads survive every host's FFI. This
    // catches JavaScript-number rounding in Workers bindings.
    let low: Row = [
        ("id".into(), i64::MIN.into()),
        ("data".into(), Value::Bytes(vec![])),
    ]
    .into();
    let high: Row = [
        ("id".into(), i64::MAX.into()),
        ("data".into(), Value::Bytes(vec![0, 255, 17])),
    ]
    .into();
    let results = store
        .transaction(tx(vec![
            S::Insert {
                table: BINARY,
                row: high.clone(),
            },
            S::Insert {
                table: BINARY,
                row: low.clone(),
            },
            S::Select(Query::new(BINARY)),
            S::Select(Query::new(BINARY).matching(vec![P::gt("id", i64::MAX - 1)])),
        ]))
        .await
        .unwrap();
    assert_eq!(results[2], vec![low, high.clone()]);
    assert_eq!(results[3], vec![high]);
}
