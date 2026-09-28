use snap_store::*;
use snap_store::{Kind, Row, Value};

#[derive(Default)]
struct Disk(Vec<Row>);
impl Backend for Disk {
    fn load(&mut self, _: &Table) -> Result<Vec<Row>, Error> {
        Ok(self.0.clone())
    }
    fn commit(&mut self, writes: &[Write]) -> Result<(), CommitError> {
        for write in writes {
            if let Write::Insert { row, .. } = write {
                self.0.push(row.clone());
            }
        }
        Ok(())
    }
}

fn schema() -> Catalog {
    Catalog::new(vec![Table {
        name: "accounts.users".into(),
        columns: vec![Column {
            name: "id".into(),
            kind: Kind::Integer,
        }],
        primary: vec!["id".into()],
        indexes: vec![],
        foreign: vec![],
    }])
    .unwrap()
}

#[test]
fn a_miss_poisoning_an_attempt_discards_even_writes_before_it() {
    let mut store = Store::new(schema(), Disk::default()).unwrap();
    let result = store.run("signup", |tx| {
        tx.insert(
            "accounts.users",
            Row::from([("id".into(), Value::Integer(1))]),
        )?;
        // Even a handler that accidentally catches the miss cannot commit.
        let _ = tx.get("accounts.users", &[Value::Integer(2)]);
        Ok(())
    });
    assert!(matches!(result, Err(Error::Miss(_))));
    assert_eq!(store.misses().count, 1);
    store.load("accounts.users").unwrap();
    let result = store
        .run("lookup", |tx| {
            tx.get("accounts.users", &[Value::Integer(1)])
        })
        .unwrap();
    assert_eq!(result.value, None);
}

#[test]
fn known_absence_and_staged_writes_are_hits_but_unknown_keys_are_misses() {
    let mut store = Store::new(schema(), Disk::default()).unwrap();
    store
        .run("insert", |tx| {
            tx.insert("accounts.users", Row::from([("id".into(), 3.into())]))?;
            assert!(tx.get("accounts.users", &[3.into()])?.is_some());
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .run("hot", |tx| tx.get("accounts.users", &[3.into()]))
            .unwrap()
            .value
            .is_some()
    );
    assert!(matches!(
        store.run("cold", |tx| tx.get("accounts.users", &[4.into()])),
        Err(Error::Miss(_))
    ));
    store.load("accounts.users").unwrap();
    assert_eq!(
        store
            .run("nx", |tx| tx.get("accounts.users", &[4.into()]))
            .unwrap()
            .value,
        None
    );
    assert!(
        store
            .run("loaded", |tx| tx.get("accounts.users", &[3.into()]))
            .unwrap()
            .value
            .is_some()
    );
}

#[test]
fn unknown_commit_outcome_fences_reads_loads_and_further_writes() {
    struct LostCommit;
    impl Backend for LostCommit {
        fn load(&mut self, _: &Table) -> Result<Vec<Row>, Error> {
            Ok(vec![])
        }
        fn commit(&mut self, _: &[Write]) -> Result<(), CommitError> {
            Err(CommitError::Indeterminate)
        }
    }
    let mut store = Store::new(schema(), LostCommit).unwrap();
    assert!(matches!(
        store.run("insert", |tx| tx
            .insert("accounts.users", Row::from([("id".into(), 3.into())]))),
        Err(Error::Indeterminate)
    ));
    assert!(matches!(
        store.run("read", |tx| tx.get("accounts.users", &[3.into()])),
        Err(Error::Indeterminate)
    ));
    assert_eq!(store.load("accounts.users"), Err(Error::Indeterminate));
}

#[test]
fn panicking_operation_cannot_publish_its_scratch() {
    let mut store = Store::new(schema(), Disk::default()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<Committed<()>, _> = store.run("panic", |tx| {
            tx.insert("accounts.users", Row::from([("id".into(), 1.into())]))?;
            panic!("application panic")
        });
    }));
    assert!(result.is_err());
    store.load("accounts.users").unwrap();
    assert!(
        store
            .run("verify", |tx| tx.get("accounts.users", &[1.into()]))
            .unwrap()
            .value
            .is_none()
    );
}

#[test]
fn a_backend_panic_during_commit_fences_the_unknown_outcome() {
    struct PanicsAfterWrite;
    impl Backend for PanicsAfterWrite {
        fn load(&mut self, _: &Table) -> Result<Vec<Row>, Error> {
            Ok(vec![])
        }
        fn commit(&mut self, _: &[Write]) -> Result<(), CommitError> {
            panic!("host lost control after issuing a commit")
        }
    }
    let mut store = Store::new(schema(), PanicsAfterWrite).unwrap();
    store.load("accounts.users").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.run("panic", |tx| {
            tx.insert("accounts.users", Row::from([("id".into(), 1.into())]))
        });
    }));
    assert!(result.is_err());
    assert!(matches!(
        store.run("read", |tx| tx.get("accounts.users", &[1.into()])),
        Err(Error::Indeterminate)
    ));
}

#[test]
fn committed_changes_coalesce_and_publish_only_the_net_state() {
    let mut store = Store::new(schema(), Disk::default()).unwrap();
    store.load("accounts.users").unwrap();
    let row = Row::from([("id".into(), 1.into())]);
    let committed = store
        .run("create", |tx| {
            tx.insert("accounts.users", row.clone())?;
            tx.delete("accounts.users", &[1.into()])?;
            tx.insert("accounts.users", row.clone())
        })
        .unwrap();
    assert_eq!(
        committed.changes,
        vec![RowChange {
            table: "accounts.users".into(),
            key: vec![1.into()],
            before: None,
            after: Some(row.clone()),
        }]
    );
    let committed = store
        .run("unchanged", |tx| {
            tx.delete("accounts.users", &[1.into()])?;
            tx.insert("accounts.users", row.clone())
        })
        .unwrap();
    assert!(committed.changes.is_empty());
    let committed = store
        .run("delete", |tx| tx.delete("accounts.users", &[1.into()]))
        .unwrap();
    assert_eq!(
        committed.changes,
        vec![RowChange {
            table: "accounts.users".into(),
            key: vec![1.into()],
            before: Some(row),
            after: None,
        }]
    );
}

#[test]
fn releasing_residency_does_not_delete_rows_or_claim_complete_indexes() {
    let mut store = Store::new(
        schema(),
        Disk(vec![
            Row::from([("id".into(), 1.into())]),
            Row::from([("id".into(), 2.into())]),
        ]),
    )
    .unwrap();
    let keys = std::collections::BTreeSet::from([vec![1.into()], vec![3.into()]]);
    store.load_keys("accounts.users", &keys).unwrap();
    assert!(
        store
            .run("one", |tx| tx.get("accounts.users", &[1.into()]))
            .unwrap()
            .value
            .is_some()
    );
    assert!(
        store
            .run("absent", |tx| tx.get("accounts.users", &[3.into()]))
            .unwrap()
            .value
            .is_none()
    );
    assert!(matches!(
        store.run("two", |tx| tx.get("accounts.users", &[2.into()])),
        Err(Error::Miss(_))
    ));
    assert!(matches!(
        store.run("index", |tx| tx.find("accounts.users", "primary", &[])),
        Err(Error::Miss(_))
    ));
    store
        .retain_keys("accounts.users", &Default::default())
        .unwrap();
    assert!(matches!(
        store.run("released", |tx| tx.get("accounts.users", &[1.into()])),
        Err(Error::Miss(_))
    ));
    store.load("accounts.users").unwrap();
    assert_eq!(
        store
            .run("disk", |tx| tx.find("accounts.users", "primary", &[]))
            .unwrap()
            .value
            .len(),
        2
    );
}
