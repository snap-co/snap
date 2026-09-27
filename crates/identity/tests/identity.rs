mod support;
use snap_identity::Identity;
use snap_store::{Error, Value};
use support::{Fake, store};

#[test]
fn session_management_is_owner_scoped_and_summaries_are_not_credentials() {
    use snap_identity::Crypto;
    let mut store = store(true);
    let identity = Identity::default();
    let mut crypto = Fake::default();
    let first = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut crypto, "a@example.test", "password1", 0)
        })
        .unwrap()
        .value;
    let second = store
        .run("login", |tx| {
            identity.login(tx, &mut crypto, "a@example.test", "password1", 1)
        })
        .unwrap()
        .value;
    let other = store
        .run("other", |tx| {
            identity.enroll(tx, &mut crypto, "b@example.test", "password2", 1)
        })
        .unwrap()
        .value;
    let summaries = store
        .run("sessions", |tx| {
            identity.sessions(tx, &crypto, &first.bearer, 2)
        })
        .unwrap()
        .value;
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries.iter().filter(|s| s.current).count(), 1);
    let current = summaries.iter().find(|s| s.current).unwrap();
    assert_ne!(current.id, first.bearer);
    assert!(matches!(
        store.run("not-a-bearer", |tx| identity.resolve(
            tx,
            &crypto,
            &current.id,
            2
        )),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("cross-owner-revoke", |tx| identity.revoke_session(
            tx,
            &crypto,
            &other.bearer,
            &current.id,
            2
        )),
        Err(Error::NotFound)
    ));
    let digest = crypto.digest(&first.bearer);
    assert_eq!(
        store
            .run("internal-reference", |tx| identity
                .resolve_digest(tx, &digest, 2))
            .unwrap()
            .value
            .identity,
        first.session.identity
    );
    store
        .run("others", |tx| {
            identity.revoke_scope(tx, &crypto, &first.bearer, "others", 2)
        })
        .unwrap();
    assert!(matches!(
        store.run("other-revoked", |tx| identity.resolve(
            tx,
            &crypto,
            &second.bearer,
            2
        )),
        Err(Error::NotFound)
    ));
    store
        .run("unrelated-live", |tx| {
            identity.resolve(tx, &crypto, &other.bearer, 2)
        })
        .unwrap();
    assert_eq!(
        store
            .run("labels", |tx| identity.credentials(
                tx,
                &crypto,
                &first.bearer,
                2
            ))
            .unwrap()
            .value,
        vec!["a@example.test"]
    );
    store
        .run("all", |tx| {
            identity.revoke_scope(tx, &crypto, &first.bearer, "all", 2)
        })
        .unwrap();
    assert!(matches!(
        store.run("grant-reference-revoked", |tx| identity
            .resolve_digest(tx, &digest, 2)),
        Err(Error::NotFound)
    ));
}

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
