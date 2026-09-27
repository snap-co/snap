mod support;
use snap_identity::Identity;
use snap_store::{Error, Value};
use support::{Fake, store};

#[test]
fn credentials_sessions_and_expiry_are_transactional() {
    let mut store = store(true);
    let identity = Identity::new(10).unwrap();
    let mut crypto = Fake::default();
    let first = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut crypto, " Alice@Example.com ", "password1", 100)
        })
        .unwrap()
        .value;
    assert!(matches!(
        store.run("duplicate", |tx| identity.enroll(
            tx,
            &mut crypto,
            "alice@example.com",
            "password2",
            100
        )),
        Err(Error::Constraint)
    ));
    assert!(matches!(
        store.run("bad", |tx| identity.login(
            tx,
            &mut crypto,
            "alice@example.com",
            "wrongpass",
            100
        )),
        Err(Error::NotFound)
    ));
    let second = store
        .run("login", |tx| {
            identity.login(tx, &mut crypto, "ALICE@example.com", "password1", 101)
        })
        .unwrap()
        .value;
    assert_eq!(first.session.identity, second.session.identity);
    assert_ne!(first.bearer, second.bearer);
    store
        .run("revoke", |tx| {
            identity.revoke(tx, &crypto, &first.bearer, 102)
        })
        .unwrap();
    assert!(matches!(
        store.run("revoked", |tx| identity.resolve(
            tx,
            &crypto,
            &first.bearer,
            102
        )),
        Err(Error::NotFound)
    ));
    store
        .run("live", |tx| {
            identity.resolve(tx, &crypto, &second.bearer, 110)
        })
        .unwrap();
    assert!(matches!(
        store.run("expired", |tx| identity.resolve(
            tx,
            &crypto,
            &second.bearer,
            111
        )),
        Err(Error::NotFound)
    ));
}

#[test]
fn cold_reads_and_late_failures_cannot_partially_enroll() {
    let mut store = store(false);
    let identity = Identity::default();
    let mut crypto = Fake::default();
    assert!(matches!(
        store.run("cold", |tx| identity.enroll(
            tx,
            &mut crypto,
            "a@b",
            "password1",
            0
        )),
        Err(Error::Miss(_))
    ));
    store.load(snap_identity::TABLES[1]).unwrap();
    assert!(matches!(
        store.run("late-miss", |tx| {
            let issued = identity.enroll(tx, &mut crypto, "a@b", "password1", 0)?;
            let _ = tx.get(snap_identity::TABLES[0], &[Value::Text("unknown".into())]);
            Ok(issued)
        }),
        Err(Error::Miss(_))
    ));
    for table in snap_identity::TABLES {
        store.load(table).unwrap();
    }
    assert!(matches!(
        store.run("aborted", |tx| {
            identity.enroll(tx, &mut crypto, "a@b", "password1", 0)?;
            Err::<(), _>(Error::Unavailable)
        }),
        Err(Error::Unavailable)
    ));
    store
        .run("retry-explicitly", |tx| {
            identity.enroll(tx, &mut crypto, "a@b", "password1", 0)
        })
        .unwrap();
}

#[test]
fn credentials_and_revocation_survive_reopen() {
    let path = std::env::temp_dir().join(format!("snap-identity-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    snap_sqlite::migrate(&path, &[support::migration()]).unwrap();
    let mut store = snap_sqlite::Sqlite::open(&path).unwrap();
    for table in snap_identity::TABLES {
        store.load(table).unwrap();
    }
    let mut crypto = Fake::default();
    let identity = Identity::default();
    let issued = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut crypto, "a@b", "password1", 0)
        })
        .unwrap()
        .value;
    drop(store);
    let mut store = snap_sqlite::Sqlite::open(&path).unwrap();
    for table in snap_identity::TABLES {
        store.load(table).unwrap();
    }
    store
        .run("resolve", |tx| {
            identity.resolve(tx, &crypto, &issued.bearer, 1)
        })
        .unwrap();
    store
        .run("revoke", |tx| {
            identity.revoke(tx, &crypto, &issued.bearer, 1)
        })
        .unwrap();
    drop(store);
    let mut store = snap_sqlite::Sqlite::open(&path).unwrap();
    for table in snap_identity::TABLES {
        store.load(table).unwrap();
    }
    assert!(matches!(
        store.run("resolve", |tx| identity.resolve(
            tx,
            &crypto,
            &issued.bearer,
            2
        )),
        Err(Error::NotFound)
    ));
    store
        .run("login", |tx| {
            identity.login(tx, &mut crypto, "a@b", "password1", 2)
        })
        .unwrap();
    drop(store);
    std::fs::remove_file(path).unwrap();
}
