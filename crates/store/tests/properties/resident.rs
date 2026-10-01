//! A plain record model checks public Store outcomes against actual SQLite.
//! The model has no indexes, residency implementation, or SQL translation.
use hegel::{TestCase, generators as gs};
use snap_store::{Kind, Row};
use snap_store::{migration::*, *};
use snap_store_sqlite::Sqlite;
use std::collections::BTreeMap;

fn migrations() -> Vec<Migration> {
    vec![Migration {
        id: "0001_model".into(),
        changes: ["left.records", "right.records", "cold.records"]
            .into_iter()
            .map(|name| Change::CreateTable {
                table: Table {
                    name: name.into(),
                    columns: vec![
                        Column {
                            name: "tenant".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "id".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "group".into(),
                            kind: Kind::Integer,
                        },
                    ],
                    primary: vec!["tenant".into(), "id".into()],
                    indexes: vec![Index {
                        name: "group_tenant".into(),
                        columns: vec!["group".into(), "tenant".into()],
                        unique: false,
                    }],
                    foreign: vec![],
                },
            })
            .collect(),
    }]
}
fn row(key: (i64, i64), group: i64) -> Row {
    Row::from([
        ("tenant".into(), key.0.into()),
        ("id".into(), key.1.into()),
        ("group".into(), group.into()),
    ])
}

#[hegel::test]
fn transaction_histories_match_a_record_model(tc: TestCase) {
    let mut store = Sqlite::memory(&migrations()).unwrap();
    for table in ["left.records", "right.records"] {
        store.load(table).unwrap();
    }
    let mut model = BTreeMap::<(i64, i64), i64>::new();
    let steps = tc.draw(gs::integers::<usize>().min_value(1).max_value(80));
    for _ in 0..steps {
        let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
        let edits: Vec<_> = (0..count)
            .map(|_| {
                (
                    tc.draw(gs::integers::<u8>().max_value(2)),
                    (
                        tc.draw(gs::integers::<i64>().max_value(2).min_value(0)),
                        tc.draw(gs::integers::<i64>().max_value(5).min_value(0)),
                    ),
                    tc.draw(gs::integers::<i64>().max_value(3).min_value(0)),
                )
            })
            .collect();
        let abort = tc.draw(gs::integers::<u8>().max_value(4));
        tc.note(&format!("edits={edits:?} abort={abort}"));
        let mut candidate = model.clone();
        let mut expected = Ok(());
        for (op, key, group) in &edits {
            match op {
                0 if candidate.contains_key(key) => {
                    expected = Err(Error::Constraint);
                    break;
                }
                1 if !candidate.contains_key(key) => {
                    expected = Err(Error::NotFound);
                    break;
                }
                0 | 1 => {
                    candidate.insert(*key, *group);
                }
                _ => {
                    candidate.remove(key);
                }
            }
        }
        if expected.is_ok() && abort == 0 {
            expected = Err(Error::Unavailable);
        }
        if expected.is_ok() && abort == 1 {
            expected = Err(Error::Miss(Lookup {
                table: "cold.records".into(),
                index: "primary".into(),
                prefix: vec![0.into(), 0.into()],
            }));
        }
        let actual = store
            .run("generated", |tx| {
                for (op, key, group) in &edits {
                    for table in ["left.records", "right.records"] {
                        let key_values = [key.0.into(), key.1.into()];
                        match op {
                            0 => tx.insert(table, row(*key, *group))?,
                            1 => tx.update(
                                table,
                                &key_values,
                                Row::from([("group".into(), (*group).into())]),
                            )?,
                            _ => {
                                tx.delete(table, &key_values)?;
                            }
                        }
                        if *op != 2 {
                            assert_eq!(tx.get(table, &key_values)?, Some(row(*key, *group)));
                        }
                    }
                }
                if abort == 0 {
                    return Err(Error::Unavailable);
                }
                if abort == 1 {
                    // Catching a miss must STILL prevent commit.
                    let _ = tx.get("cold.records", &[0.into(), 0.into()]);
                }
                Ok(())
            })
            .map(|c| {
                let mut keys: Vec<_> = model.keys().chain(candidate.keys()).copied().collect();
                keys.sort();
                keys.dedup();
                let changes: Vec<_> = ["left.records", "right.records"]
                    .into_iter()
                    .flat_map(|table| {
                        keys.iter().filter_map(|key| {
                            let before = model.get(key).map(|g| row(*key, *g));
                            let after = candidate.get(key).map(|g| row(*key, *g));
                            (before != after).then_some(RowChange {
                                table: table.into(),
                                key: vec![key.0.into(), key.1.into()],
                                before,
                                after,
                            })
                        })
                    })
                    .collect();
                assert_eq!(c.changes, changes);
                c.value
            });
        assert_eq!(actual, expected);
        if expected.is_ok() {
            model = candidate;
            tc.event("committed");
        } else {
            tc.event("rolled back");
        }
        for table in ["left.records", "right.records"] {
            let expected: Vec<_> = model.iter().map(|(key, group)| row(*key, *group)).collect();
            assert_eq!(
                store
                    .run("all", |tx| tx.find(table, "primary", &[]))
                    .unwrap()
                    .value,
                expected
            );
            for group in 0..=3 {
                let expected: Vec<_> = model
                    .iter()
                    .filter(|(_, g)| **g == group)
                    .map(|(key, g)| row(*key, *g))
                    .collect();
                assert_eq!(
                    store
                        .run("index", |tx| tx.find(
                            table,
                            "group_tenant",
                            &[group.into()]
                        ))
                        .unwrap()
                        .value,
                    expected
                );
            }
            if tc.draw(gs::booleans()) {
                store.load(table).unwrap();
                assert_eq!(
                    store
                        .run("durable", |tx| tx.find(table, "primary", &[]))
                        .unwrap()
                        .value,
                    expected
                );
                tc.event("verified disk reload");
            }
        }
    }
}

#[hegel::test]
fn cold_inserts_are_hits_without_claiming_a_complete_index(tc: TestCase) {
    let mut store = Sqlite::memory(&migrations()).unwrap();
    let ids = tc.draw(gs::vecs(gs::integers::<i64>().min_value(0).max_value(12)).max_size(30));
    let mut expected = BTreeMap::new();
    for id in ids {
        let key = (0, id);
        let result = store.run("insert", |tx| tx.insert("left.records", row(key, id % 3)));
        if expected.insert(id, row(key, id % 3)).is_some() {
            assert!(matches!(result, Err(Error::Constraint)));
        } else {
            result.unwrap();
        }
        assert_eq!(
            store
                .run("hot", |tx| tx.get("left.records", &[0.into(), id.into()]))
                .unwrap()
                .value,
            Some(row(key, id % 3))
        );
    }
    assert!(matches!(
        store.run("partial index", |tx| tx.find(
            "left.records",
            "group_tenant",
            &[]
        )),
        Err(Error::Miss(_))
    ));
    assert!(matches!(
        store.run("unknown", |tx| tx
            .get("left.records", &[99.into(), 99.into()])),
        Err(Error::Miss(_))
    ));
    store.load("left.records").unwrap();
    assert_eq!(
        store
            .run("nx", |tx| tx.get("left.records", &[99.into(), 99.into()]))
            .unwrap()
            .value,
        None
    );
    assert_eq!(
        store
            .run("all", |tx| tx.find("left.records", "primary", &[]))
            .unwrap()
            .value,
        expected.into_values().collect::<Vec<_>>()
    );
}

#[hegel::test]
fn rejection_and_lost_commit_acknowledgement_never_serve_stale_memory(tc: TestCase) {
    use std::{cell::RefCell, rc::Rc};
    struct FaultDisk {
        durable: Rc<RefCell<Vec<Row>>>,
        fault: u8,
    }
    impl Backend for FaultDisk {
        fn load(&mut self, _: &Table) -> Result<Vec<Row>, Error> {
            Ok(self.durable.borrow().clone())
        }
        fn commit(&mut self, writes: &[Write]) -> Result<(), CommitError> {
            if self.fault == 1 {
                return Err(CommitError::Rejected(Error::Unavailable));
            }
            for write in writes {
                if let Write::Insert { row, .. } = write {
                    self.durable.borrow_mut().push(row.clone());
                }
            }
            if self.fault == 2 {
                Err(CommitError::Indeterminate)
            } else {
                Ok(())
            }
        }
    }
    let fault = tc.draw(gs::integers::<u8>().max_value(2));
    let group = tc.draw(gs::integers::<i64>());
    let catalog = migrations()[0].apply(&Catalog::default()).unwrap();
    let durable = Rc::new(RefCell::new(vec![]));
    let mut store = Store::new(
        catalog.clone(),
        FaultDisk {
            durable: durable.clone(),
            fault,
        },
    )
    .unwrap();
    store.load("left.records").unwrap();
    let result = store.run("commit", |tx| tx.insert("left.records", row((1, 1), group)));
    match fault {
        0 => {
            result.unwrap();
            assert_eq!(
                store
                    .run("read", |tx| tx.get("left.records", &[1.into(), 1.into()]))
                    .unwrap()
                    .value,
                Some(row((1, 1), group))
            );
        }
        1 => {
            assert!(matches!(result, Err(Error::Unavailable)));
            assert_eq!(
                store
                    .run("read", |tx| tx.get("left.records", &[1.into(), 1.into()]))
                    .unwrap()
                    .value,
                None
            );
        }
        _ => {
            assert!(matches!(result, Err(Error::Indeterminate)));
            assert!(matches!(
                store.run("read", |tx| tx.get("left.records", &[1.into(), 1.into()])),
                Err(Error::Indeterminate)
            ));
        }
    }
    drop(store);
    let mut recovered = Store::new(catalog, FaultDisk { durable, fault: 0 }).unwrap();
    recovered.load("left.records").unwrap();
    assert_eq!(
        recovered
            .run("recovered", |tx| tx
                .get("left.records", &[1.into(), 1.into()]))
            .unwrap()
            .value,
        (fault != 1).then(|| row((1, 1), group))
    );
}

#[hegel::test]
fn migration_indexes_require_their_columns_at_each_step(tc: TestCase) {
    let initial = migrations().remove(0);
    let catalog = initial.apply(&Catalog::default()).unwrap();
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
    let mut pending: Vec<_> = (0..count)
        .flat_map(|id| [(false, id), (true, id)])
        .collect();
    let mut available = std::collections::BTreeSet::new();
    let mut valid = true;
    let mut changes = Vec::new();
    while !pending.is_empty() {
        let choice = tc.draw(gs::integers::<usize>().max_value(pending.len() - 1));
        let (index, id) = pending.remove(choice);
        let name = format!("field_{id}");
        if index {
            valid &= available.contains(&id);
            changes.push(Change::CreateIndex {
                table: "left.records".into(),
                index: Index {
                    name: format!("index_{id}"),
                    columns: vec![name],
                    unique: false,
                },
            });
        } else {
            available.insert(id);
            changes.push(Change::AddColumn {
                table: "left.records".into(),
                column: Column {
                    name,
                    kind: Kind::Integer,
                },
                fill: 0.into(),
            });
        }
    }
    tc.note(&format!("changes={changes:?}"));
    tc.event(if valid {
        "valid ordered DDL"
    } else {
        "forward column reference rejected"
    });
    let next = Migration {
        id: "0002_indexes".into(),
        changes,
    };
    assert_eq!(next.apply(&catalog).is_ok(), valid);
    assert_eq!(Sqlite::memory(&[initial, next]).is_ok(), valid);
}
