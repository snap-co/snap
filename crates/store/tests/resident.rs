use snap_store::*;
use snap_store::{Kind, Row};

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
