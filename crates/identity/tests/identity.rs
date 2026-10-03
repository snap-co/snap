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
            identity.acquire(tx, &mut crypto, "a@example.test", "password1", 1)
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
        first.principal.identity
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
        vec![snap_identity::CredentialSummary {
            locator: "a@example.test".into(),
            label: "a@example.test".into(),
            kind: snap_identity::CredentialKind::Password,
            removable: false
        }]
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
        store.run("bad", |tx| identity.acquire(
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
            identity.acquire(tx, &mut crypto, "ALICE@example.com", "password1", 101)
        })
        .unwrap()
        .value;
    assert_eq!(first.principal.identity, second.principal.identity);
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
    snap_store_sqlite::migrate(&path, &support::migrations()).unwrap();
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
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
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
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
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
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
            identity.acquire(tx, &mut crypto, "a@b", "password1", 2)
        })
        .unwrap();
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn authentication_time_is_durable_and_independent_of_configured_lifetime() {
    let mut store = store(true);
    let mut crypto = Fake::default();
    let issued = store
        .run("enroll", |tx| {
            Identity::new(10).unwrap().enroll(
                tx,
                &mut crypto,
                "time@example.test",
                "password1",
                100,
            )
        })
        .unwrap()
        .value;
    let resolved = store
        .run("resolve", |tx| {
            Identity::new(1000)
                .unwrap()
                .resolve(tx, &crypto, &issued.bearer, 105)
        })
        .unwrap()
        .value;
    assert_eq!(resolved, issued.principal);
    assert_eq!(resolved.authenticated_at, 100);
}

#[test]
fn additive_session_migration_preserves_existing_authority_without_inventing_freshness() {
    use snap_identity::Crypto;
    let migrations = support::migrations();
    let path = tempfile_path();
    let _ = std::fs::remove_file(&path);
    snap_store_sqlite::migrate(&path, &migrations[..1]).unwrap();
    let crypto = Fake::default();
    let bearer = "a".repeat(64);
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    store.load(snap_identity::TABLES[0]).unwrap();
    store.load(snap_identity::TABLES[2]).unwrap();
    store
        .run("legacy", |tx| {
            tx.insert(
                snap_identity::TABLES[0],
                [("id".into(), "legacy".into())].into_iter().collect(),
            )?;
            tx.insert(
                snap_identity::TABLES[2],
                [
                    ("digest".into(), Value::Bytes(crypto.digest(&bearer))),
                    ("identity".into(), "legacy".into()),
                    ("expires".into(), 100.into()),
                ]
                .into_iter()
                .collect(),
            )
        })
        .unwrap();
    drop(store);
    snap_store_sqlite::migrate(&path, &migrations).unwrap();
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    Identity::default().data().prepare(&mut store).unwrap();
    let principal = store
        .run("resolve", |tx| {
            Identity::default().resolve(tx, &crypto, &bearer, 99)
        })
        .unwrap()
        .value;
    assert_eq!(principal.identity, "legacy");
    assert_eq!(principal.authenticated_at, 0);
    assert!(matches!(
        store.run("expired", |tx| Identity::default()
            .resolve(tx, &crypto, &bearer, 100)),
        Err(Error::NotFound)
    ));
    drop(store);
    std::fs::remove_file(path).unwrap();
}
fn tempfile_path() -> std::path::PathBuf {
    named_path("session")
}
/// Migration tests rewrite the same file, so each claims its own path rather
/// than serializing on a shared one.
fn named_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "snap-identity-migration-{label}-{}.sqlite",
        std::process::id()
    ))
}

/// Credentials written before the kind column existed must still read back as
/// password credentials, and the recorded kind must be the row's own value
/// rather than an assumption about the locator.
#[test]
fn additive_credential_kind_migration_classifies_existing_rows_as_passwords() {
    use snap_identity::{Credential, CredentialKind};
    let migrations = support::migrations();
    let path = named_path("kind");
    let _ = std::fs::remove_file(&path);
    snap_store_sqlite::migrate(&path, &migrations[..2]).unwrap();
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    store.load(snap_identity::TABLES[0]).unwrap();
    store.load(snap_identity::TABLES[1]).unwrap();
    store
        .run("legacy", |tx| {
            tx.insert(
                snap_identity::TABLES[0],
                [("id".into(), "legacy".into())].into_iter().collect(),
            )?;
            tx.insert(
                snap_identity::TABLES[1],
                [
                    ("email".into(), "legacy@example.test".into()),
                    ("identity".into(), "legacy".into()),
                    ("hash".into(), "legacy-hash".into()),
                ]
                .into_iter()
                .collect(),
            )
        })
        .unwrap();
    drop(store);
    snap_store_sqlite::migrate(&path, &migrations).unwrap();
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    Credential::data().prepare(&mut store).unwrap();
    let kind = store
        .run("read-kind", |tx| {
            Ok(Credential::find(tx, "legacy@example.test")?
                .expect("migrated credential")
                .kind())
        })
        .unwrap()
        .value;
    assert_eq!(kind, CredentialKind::Password);
    assert_eq!(CredentialKind::Password.as_str(), "password");
    assert!(matches!(
        CredentialKind::parse("unknown"),
        Err(Error::Invalid)
    ));
    drop(store);
    std::fs::remove_file(path).unwrap();
}

/// A freshly enrolled credential records its kind in the row, and the summary a
/// principal reads is derived from the stored column.
#[test]
fn enrollment_records_kind_and_summaries_report_it() {
    use snap_identity::{Credential, CredentialKind, CredentialSummary};
    let mut store = store(true);
    let identity = Identity::default();
    let mut crypto = Fake::default();
    let issued = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut crypto, "kind@example.test", "password1", 0)
        })
        .unwrap()
        .value;
    let recorded = store
        .run("read", |tx| {
            Ok(Credential::find(tx, "kind@example.test")?
                .expect("enrolled credential")
                .kind())
        })
        .unwrap()
        .value;
    assert_eq!(recorded, CredentialKind::Password);
    let summaries = store
        .run("summaries", |tx| {
            identity.credentials(tx, &crypto, &issued.bearer, 1)
        })
        .unwrap()
        .value;
    assert_eq!(
        summaries,
        vec![CredentialSummary {
            locator: "kind@example.test".into(),
            label: "kind@example.test".into(),
            kind: CredentialKind::Password,
            removable: false,
        }]
    );
    drop(store);
}

#[test]
fn invalid_proofs_are_rejected_before_credential_lookup() {
    let mut store = store(true);
    let identity = Identity::default();
    let mut crypto = Fake::default();
    store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut crypto, "known@example.test", "password1", 0)
        })
        .unwrap();
    for email in ["known@example.test", "unknown@example.test"] {
        assert!(matches!(
            store.run("invalid-proof", |tx| identity.acquire(
                tx,
                &mut crypto,
                email,
                "short",
                1
            )),
            Err(Error::Invalid)
        ));
    }
}
