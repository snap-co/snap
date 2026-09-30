use authy::{Account, SESSION_LIFETIME_SECONDS};
use serde_json::json;
use snap_access::Role;
use snap_document::{
    Intent, Manifest, Reconciliation, ServerMessage,
    client::{Client, Outcome},
};
use snap_identity::{Crypto, Identity};
use snap_store::{Error as StoreError, Value};

#[derive(Default)]
struct Fake(u64);
impl Crypto for Fake {
    fn random(&mut self) -> Result<[u8; 32], StoreError> {
        self.0 += 1;
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&self.0.to_be_bytes());
        Ok(bytes)
    }
    fn hash_password(&mut self, password: &str) -> Result<String, StoreError> {
        Ok(format!("fake:{password}"))
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, StoreError> {
        Ok(hash == format!("fake:{password}"))
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        secret.as_bytes().to_vec()
    }
}

fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut all = vec![
        toml::from_str(snap_identity::MIGRATION).unwrap(),
        toml::from_str(snap_access::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::LIFECYCLE_MIGRATION).unwrap(),
        toml::from_str(authy::MIGRATION).unwrap(),
    ];
    all.sort_by(|a: &snap_store::migration::Migration, b| a.id.cmp(&b.id));
    all
}

fn all_tables() -> Vec<&'static str> {
    let mut tables = Vec::new();
    tables.extend(snap_identity::TABLES.iter().copied());
    tables.extend(snap_access::TABLES.iter().copied());
    tables.extend(snap_document::server::TABLES.iter().copied());
    tables.extend(authy::TABLES.iter().copied());
    tables
}

type Store = snap_store::Store<snap_sqlite::Sqlite>;

fn dispatch_profile(
    store: &mut Store,
    lifetime: &str,
    actor: &str,
    intent: Intent,
) -> snap_transport::Outcome {
    use snap_transport::operation::{Context, Runtime};
    let mut runtime = Runtime::default();
    for definition in snap_document::operations::definitions(std::sync::Arc::new(authy::document()))
    {
        runtime.register(definition).unwrap();
    }
    let selection = runtime.definitions().resolve("document.mutate")?;
    runtime.enqueue(
        (),
        snap_transport::Invocation {
            id: 1,
            operation: "document.mutate".into(),
            input: json!(intent),
        },
        selection,
    )?;
    let (work, call, selection) = runtime.acquire().unwrap();
    let context = Context {
        actor: Some(actor.into()),
        lifetime: Some(lifetime.into()),
        ..Context::default()
    };
    if let Err((_, error)) = runtime.accept(store, work, call, selection, context) {
        runtime.reject();
        return Err(error);
    }
    let outcome = runtime.execute(store).unwrap().outcome;
    runtime.finish();
    outcome
}

fn store_loaded() -> (Store, Fake) {
    let mut store = snap_sqlite::Sqlite::memory(&migrations()).unwrap();
    for table in all_tables() {
        store.load(table).unwrap();
    }
    (store, Fake::default())
}

fn dispatch_account(
    store: &mut Store,
    operation: &str,
    input: serde_json::Value,
) -> snap_transport::Outcome {
    use snap_transport::operation::{Context, Runtime};
    let app = authy::operations::declarations(Fake::default);
    let mut runtime = Runtime::default();
    for definition in app.preconnection.into_iter().chain(app.requests) {
        runtime.register(definition).unwrap();
    }
    let selection = runtime.definitions().resolve(operation)?;
    runtime.enqueue(
        (),
        snap_transport::Invocation {
            id: 1,
            operation: operation.into(),
            input,
        },
        selection,
    )?;
    let (work, call, selection) = runtime.acquire().unwrap();
    if let Err((_, error)) = runtime.accept(
        store,
        work,
        call,
        selection,
        Context {
            inputs: [("clock".into(), json!(1_000))].into_iter().collect(),
            ..Context::default()
        },
    ) {
        runtime.reject();
        return Err(error);
    }
    let outcome = runtime.execute(store).unwrap().outcome;
    runtime.finish();
    outcome
}

fn enroll(
    store: &mut Store,
    crypto: &mut Fake,
    email: &str,
    password: &str,
    now: i64,
) -> snap_identity::Issued {
    store
        .run("enroll", |tx| {
            authy::enroll(tx, crypto, email, password, now)
        })
        .unwrap()
        .value
}

fn current(store: &mut Store, crypto: &Fake, bearer: &str, now: i64) -> Account {
    store
        .run("current", |tx| authy::current(tx, crypto, bearer, now))
        .unwrap()
        .value
}

fn edit_intent(id: u64, document: &str, name: &str, bio: &str, revision: u64) -> Intent {
    Intent {
        id,
        document: document.into(),
        version: "1".into(),
        mutation: "edit".into(),
        args: json!({"name": name, "bio": bio, "revision": revision}),
    }
}

#[test]
fn enroll_creates_session_profile_grant_and_metadata_atomically() {
    let (mut store, crypto) = store_loaded();
    let result = dispatch_account(
        &mut store,
        "identity.enroll",
        json!({"email":" Alice@Example.com ", "password":"password1"}),
    )
    .unwrap();
    let bearer = result["bearer"].as_str().unwrap().to_owned();
    let session = store
        .inspect("persisted session", |tx| {
            Identity::default().resolve(tx, &crypto, &bearer, 1_000)
        })
        .unwrap();
    let issued = snap_identity::Issued { bearer, session };
    assert_eq!(issued.bearer.len(), 64);
    assert_eq!(issued.session.identity.len(), 64);
    assert_eq!(issued.session.expires, 1_000 + SESSION_LIFETIME_SECONDS);

    let profile = authy::profile_id(&issued.session.identity).unwrap();
    // First-32-hex grouping: deterministic UUID shape.
    assert_eq!(profile.len(), 36);
    assert_eq!(
        [
            &profile[8..9],
            &profile[13..14],
            &profile[18..19],
            &profile[23..24]
        ],
        ["-", "-", "-", "-"]
    );

    let account = current(&mut store, &crypto, &issued.bearer, 1_001);
    assert_eq!(account.identity, issued.session.identity);
    assert_eq!(account.email, "alice@example.com");
    assert_eq!(account.profile, profile);
    assert_eq!(account.authenticated_at, 1_000);
    assert_eq!(result["account"], json!(account));

    // Initial profile value preserves the email local part with empty bio.
    let snapshot = store
        .run("read", |tx| {
            authy::document().read(tx, &profile, Some(&issued.session.identity))
        })
        .unwrap()
        .value;
    assert_eq!(snapshot.kind, "authy-profile");
    assert_eq!(snapshot.version, "1");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.value, json!({"name": "alice", "bio": ""}));

    // Owner holds the pre-change Owner grant; nobody else can read.
    let role = store
        .run("role", |tx| {
            snap_access::Access::new(vec![snap_access::KindDefinition::kind("document").unwrap()])
                .unwrap()
                .role(
                    tx,
                    &snap_access::Resource::new("document", &profile).unwrap(),
                    Some(&issued.session.identity),
                    true,
                )
        })
        .unwrap()
        .value;
    assert_eq!(role, Some(Role::Owner));

    // OIDC path resolves the same metadata with caller-supplied current time.
    let by_identity = store
        .run("by-identity", |tx| {
            authy::account_by_identity(tx, &issued.session.identity, 5_000)
        })
        .unwrap()
        .value;
    assert_eq!(by_identity.email, "alice@example.com");
    assert_eq!(by_identity.profile, profile);
    assert_eq!(by_identity.authenticated_at, 5_000);
    let info = store
        .run("info", |tx| {
            authy::profile_info(tx, &issued.session.identity)
        })
        .unwrap()
        .value;
    assert_eq!(info.email, "alice@example.com");
    assert_eq!(info.profile, profile);

    // Account serializes with exactly the coordinated fields.
    let value = serde_json::to_value(&account).unwrap();
    assert_eq!(
        value,
        json!({
            "identity": issued.session.identity,
            "email": "alice@example.com",
            "profile": profile,
            "authenticated_at": 1_000,
        })
    );
}

#[test]
fn late_failure_after_enroll_discards_everything() {
    let (mut store, mut crypto) = store_loaded();
    let failed = store.run("signup", |tx| {
        authy::enroll(tx, &mut crypto, "late@example.com", "password1", 0)?;
        // Simulate a late caller failure after all module writes staged.
        Err::<(), _>(StoreError::Unavailable)
    });
    assert!(matches!(failed, Err(StoreError::Unavailable)));

    // Nothing persisted: login cannot find the credential.
    let identity = Identity::default();
    assert!(matches!(
        store.run("login", |tx| identity.login(
            tx,
            &mut crypto,
            "late@example.com",
            "password1",
            1
        )),
        Err(StoreError::NotFound)
    ));
    // Metadata absent as well.
    assert!(matches!(
        store.run("missing", |tx| authy::profile_info(
            tx,
            "0000000000000000000000000000000000000000000000000000000000000000"
        )),
        Err(StoreError::NotFound)
    ));

    // An explicit retry after the failure commits cleanly.
    let issued = enroll(&mut store, &mut crypto, "late@example.com", "password1", 2);
    let account = current(&mut store, &crypto, &issued.bearer, 3);
    assert_eq!(account.email, "late@example.com");
}

#[test]
fn cold_tables_report_miss_and_stage_nothing() {
    let mut store = snap_sqlite::Sqlite::memory(&migrations()).unwrap();
    let mut crypto = Fake::default();
    assert!(matches!(
        store.run("cold-enroll", |tx| authy::enroll(
            tx,
            &mut crypto,
            "cold@example.com",
            "password1",
            0
        )),
        Err(StoreError::Miss(_))
    ));
    // A miss poisons the attempt even when caught.
    let poisoned = store.run("poison", |tx| {
        let _ = authy::enroll(tx, &mut crypto, "cold@example.com", "password1", 0);
        Ok(())
    });
    assert!(matches!(poisoned, Err(StoreError::Miss(_))));
    for table in all_tables() {
        store.load(table).unwrap();
    }
    // Nothing from the cold attempts survived; enrollment works after loading.
    let issued = enroll(&mut store, &mut crypto, "cold@example.com", "password1", 0);
    assert_eq!(
        current(&mut store, &crypto, &issued.bearer, 1).email,
        "cold@example.com"
    );
}

#[test]
fn duplicate_email_rejected_without_new_profile() {
    let (mut store, mut crypto) = store_loaded();
    let first = enroll(
        &mut store,
        &mut crypto,
        "person@example.test",
        "original password",
        10,
    );
    let duplicate = store.run("duplicate", |tx| {
        authy::enroll(
            tx,
            &mut crypto,
            " PERSON@example.test ",
            "different password",
            11,
        )
    });
    assert!(matches!(duplicate, Err(StoreError::Constraint)));

    // Original session still resolves; no second identity was created.
    let account = current(&mut store, &crypto, &first.bearer, 12);
    assert_eq!(account.identity, first.session.identity);
    let rows = store
        .run("list", |tx| tx.find(authy::TABLES[0], "primary", &[]))
        .unwrap()
        .value;
    assert_eq!(rows.len(), 1);

    // Invalid enrollment does not claim the address either.
    assert!(matches!(
        store.run("short", |tx| authy::enroll(
            tx,
            &mut crypto,
            "fresh@example.test",
            "short",
            13
        )),
        Err(StoreError::Invalid)
    ));
    let retry = enroll(
        &mut store,
        &mut crypto,
        "fresh@example.test",
        "password sessions",
        14,
    );
    assert_ne!(retry.session.identity, first.session.identity);
}

#[test]
fn profile_is_private_and_owner_edit_succeeds() {
    let (mut store, mut crypto) = store_loaded();
    let alice = enroll(
        &mut store,
        &mut crypto,
        "alice@example.com",
        "password1",
        100,
    );
    let bob = enroll(&mut store, &mut crypto, "bob@example.com", "password1", 100);
    let alice_profile = authy::profile_id(&alice.session.identity).unwrap();

    // Bob's synchronization policy excludes Alice's private profile. Resident
    // reads inside accepted handlers do not reinterpret the caller's authority.
    let manifest = store
        .run("manifest", |tx| {
            authy::document().access_guard().manifest(
                tx,
                "boot:bob",
                &bob.session.identity,
                &Manifest::default(),
            )
        })
        .unwrap()
        .value;
    assert!(manifest.documents.iter().all(|d| d.id != alice_profile));

    // Bob's edit is denied without a document write (completion carries Denied).
    let denied = dispatch_profile(
        &mut store,
        "boot:bob",
        &bob.session.identity,
        edit_intent(1, &alice_profile, "Bob", "hi", 1),
    );
    assert_eq!(
        denied,
        Err(snap_transport::Error::Application(json!(
            snap_document::Error::Denied
        )))
    );

    // Owner edit succeeds and advances exactly one revision.
    let edited = store
        .run("edit", |tx| {
            authy::document().mutate(
                tx,
                "boot:alice",
                &alice.session.identity,
                &edit_intent(1, &alice_profile, "  Alice Cooper  ", "Singer", 1),
            )
        })
        .unwrap()
        .value;
    let next = edited.completion.result.unwrap().unwrap();
    assert_eq!(next.revision, 2);
    assert_eq!(next.value, json!({"name": "Alice Cooper", "bio": "Singer"}));
    assert!(edited.replication.is_some());

    // Invalid edits are domain Invalid completions, not Store misuse. They use
    // the current revision so the guard passes and the apply is exercised.
    // Each case uses a distinct lifetime+id so receipts never conflict.
    for (index, (name, bio)) in [("", "x"), ("   ", "x"), (&"a".repeat(101), "x")]
        .iter()
        .enumerate()
    {
        let rejected = store
            .run("invalid-name", |tx| {
                authy::document().mutate(
                    tx,
                    &format!("boot:alice-invalid-{index}"),
                    &alice.session.identity,
                    &edit_intent(10 + index as u64, &alice_profile, name, bio, 2),
                )
            })
            .unwrap()
            .value;
        assert!(
            matches!(
                rejected.completion.result,
                Err(snap_document::Error::Invalid)
            ),
            "{name:?}"
        );
    }
    // Missing revision fails the app guard, so it reports Denied (no write);
    // the unguarded apply never runs.
    let missing_revision = dispatch_profile(
        &mut store,
        "boot:alice-2b",
        &alice.session.identity,
        Intent {
            id: 7,
            document: alice_profile.clone(),
            version: "1".into(),
            mutation: "edit".into(),
            args: json!({"name": "Alice", "bio": "x"}),
        },
    );
    assert_eq!(
        missing_revision,
        Err(snap_transport::Error::Application(json!(
            snap_document::Error::Denied
        )))
    );
    let long_bio = "b".repeat(2001);
    let rejected = store
        .run("invalid-bio", |tx| {
            authy::document().mutate(
                tx,
                "boot:alice-3",
                &alice.session.identity,
                &edit_intent(3, &alice_profile, "Alice", &long_bio, 2),
            )
        })
        .unwrap()
        .value;
    assert!(matches!(
        rejected.completion.result,
        Err(snap_document::Error::Invalid)
    ));

    // Failed edits left the committed value at the successful edit.
    let kept = store
        .run("read", |tx| {
            authy::document().read(tx, &alice_profile, Some(&alice.session.identity))
        })
        .unwrap()
        .value;
    assert_eq!(kept.revision, 2);
    assert_eq!(kept.value, json!({"name": "Alice Cooper", "bio": "Singer"}));
}

#[test]
fn stale_profile_edit_is_rejected_without_a_write() {
    let (mut store, mut crypto) = store_loaded();
    let alice = enroll(
        &mut store,
        &mut crypto,
        "stale@example.com",
        "password1",
        100,
    );
    let profile = authy::profile_id(&alice.session.identity).unwrap();

    // First causal edit at revision 1 commits revision 2.
    let first = store
        .run("edit-1", |tx| {
            authy::document().mutate(
                tx,
                "boot:stale",
                &alice.session.identity,
                &edit_intent(1, &profile, "First", "one", 1),
            )
        })
        .unwrap()
        .value;
    assert_eq!(
        first
            .completion
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .revision,
        2
    );

    // A concurrent edit still carrying revision 1 is stale: the app guard
    // denies it without a document write, even though the actor is the owner.
    // Document itself keeps its latest-state policy; this freshness check is
    // app-specific.
    let stale = dispatch_profile(
        &mut store,
        "boot:stale",
        &alice.session.identity,
        edit_intent(2, &profile, "Stale", "late", 1),
    );
    assert_eq!(
        stale,
        Err(snap_transport::Error::Application(json!(
            snap_document::Error::Denied
        )))
    );

    // The committed value is untouched by the stale attempt.
    let kept = store
        .run("read", |tx| {
            authy::document().read(tx, &profile, Some(&alice.session.identity))
        })
        .unwrap()
        .value;
    assert_eq!(kept.revision, 2);
    assert_eq!(kept.value, json!({"name": "First", "bio": "one"}));

    // The next causal edit carries the fresh revision 2 and commits revision 3,
    // so ACK-paced consecutive edits stay causal.
    let second = store
        .run("edit-2", |tx| {
            authy::document().mutate(
                tx,
                "boot:stale",
                &alice.session.identity,
                &edit_intent(3, &profile, "Second", "two", 2),
            )
        })
        .unwrap()
        .value;
    let next = second.completion.result.unwrap().unwrap();
    assert_eq!(next.revision, 3);
    assert_eq!(next.value, json!({"name": "Second", "bio": "two"}));
}

#[test]
fn client_sdk_drives_optimistic_profile_edits() {
    let (mut store, mut crypto) = store_loaded();
    let alice = enroll(&mut store, &mut crypto, "sdk@example.com", "password1", 50);
    let profile = authy::profile_id(&alice.session.identity).unwrap();
    let registry = authy::registry();

    let base = store
        .run("read", |tx| {
            authy::document().read(tx, &profile, Some(&alice.session.identity))
        })
        .unwrap()
        .value;

    // Install authoritative state, enqueue an optimistic edit, and observe the
    // projected view before the server commits. The revision comes from the
    // projected snapshot, keeping ACK-paced edits causal.
    let mut client = Client::new(alice.session.identity.clone());
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                unchanged: vec![],
                documents: vec![base],
                completed: vec![],
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    let base_revision = client.get(&profile).unwrap().revision;
    let id = client
        .enqueue(
            &registry,
            &profile,
            "edit",
            json!({"name": "Sdk", "bio": "optimistic", "revision": base_revision}),
        )
        .unwrap();
    assert_eq!(
        client.get(&profile).unwrap().value,
        json!({"name": "Sdk", "bio": "optimistic"})
    );
    let submission = client.next_submission().unwrap();
    let intent = match submission {
        snap_document::ClientMessage::Mutate(intent) => intent,
        other => panic!("unexpected submission: {other:?}"),
    };
    assert_eq!(intent.id, id);
    assert_eq!(intent.document, profile);

    // Commit through the server, then apply the authoritative completion as one
    // coherent publication without double-applying the optimistic entry.
    let result = store
        .run("commit", |tx| {
            authy::document().mutate(tx, "boot:sdk", &alice.session.identity, &intent)
        })
        .unwrap()
        .value;
    assert!(!result.replayed);
    let outcome = client
        .handle(&registry, ServerMessage::Completed(result.completion))
        .unwrap();
    assert!(matches!(outcome, Outcome::Completed { .. }));
    assert!(client.pending().is_empty());
    assert_eq!(client.get(&profile).unwrap().revision, 2);
    assert_eq!(
        client.get(&profile).unwrap().value,
        json!({"name": "Sdk", "bio": "optimistic"})
    );

    // A second causal edit takes the fresh revision 2 from the projected view
    // and commits revision 3.
    let fresh = client.get(&profile).unwrap().revision;
    client
        .enqueue(
            &registry,
            &profile,
            "edit",
            json!({"name": "Sdk2", "bio": "again", "revision": fresh}),
        )
        .unwrap();
    let intent = match client.next_submission().unwrap() {
        snap_document::ClientMessage::Mutate(intent) => intent,
        other => panic!("unexpected submission: {other:?}"),
    };
    let result = store
        .run("commit-2", |tx| {
            authy::document().mutate(tx, "boot:sdk", &alice.session.identity, &intent)
        })
        .unwrap()
        .value;
    assert!(result.completion.result.is_ok());
    let outcome = client
        .handle(&registry, ServerMessage::Completed(result.completion))
        .unwrap();
    assert!(matches!(outcome, Outcome::Completed { .. }));
    assert_eq!(client.get(&profile).unwrap().revision, 3);
    assert_eq!(
        client.get(&profile).unwrap().value,
        json!({"name": "Sdk2", "bio": "again"})
    );
}

#[test]
fn current_login_logout_and_expiry_follow_session_lifetime() {
    let (mut store, mut crypto) = store_loaded();
    let identity = Identity::default();
    let first = enroll(
        &mut store,
        &mut crypto,
        "session@example.com",
        "password1",
        100,
    );
    let second = store
        .run("login", |tx| {
            identity.login(tx, &mut crypto, "SESSION@example.com", "password1", 101)
        })
        .unwrap()
        .value;
    assert_eq!(first.session.identity, second.session.identity);
    assert_ne!(first.bearer, second.bearer);

    // Both bearers resolve to the same account; auth time tracks issuance.
    let a = current(&mut store, &crypto, &first.bearer, 102);
    let b = current(&mut store, &crypto, &second.bearer, 102);
    assert_eq!(a.identity, b.identity);
    assert_eq!(a.profile, b.profile);
    assert_eq!(a.authenticated_at, 100);
    assert_eq!(b.authenticated_at, 101);

    // Logout revokes only the supplied session; the other bearer keeps working.
    store
        .run("logout", |tx| {
            identity.revoke(tx, &crypto, &first.bearer, 103)
        })
        .unwrap();
    assert!(matches!(
        store.run("revoked", |tx| authy::current(
            tx,
            &crypto,
            &first.bearer,
            103
        )),
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        current(&mut store, &crypto, &second.bearer, 103).identity,
        second.session.identity
    );

    // Expiry at now >= expires grants no authority.
    assert!(matches!(
        store.run("expired", |tx| authy::current(
            tx,
            &crypto,
            &second.bearer,
            second.session.expires
        )),
        Err(StoreError::NotFound)
    ));
    // Unknown bearer and unknown identity are NotFound, not Miss.
    assert!(matches!(
        store.run("unknown", |tx| authy::current(
            tx,
            &crypto,
            &"0".repeat(64),
            103
        )),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.run("unknown-id", |tx| authy::account_by_identity(
            tx,
            &"f".repeat(64),
            103
        )),
        Err(StoreError::NotFound)
    ));
}

#[test]
fn sessions_and_profiles_survive_reopen() {
    let path = std::path::PathBuf::from(format!(
        "/tmp/opencode/snap-authy-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    snap_sqlite::migrate(&path, &migrations()).unwrap();
    let (bearer, identity_id, profile) = {
        let mut store = snap_sqlite::Sqlite::open(&path).unwrap();
        for table in all_tables() {
            store.load(table).unwrap();
        }
        let mut crypto = Fake::default();
        let issued = enroll(
            &mut store,
            &mut crypto,
            "persist@example.com",
            "password1",
            7,
        );
        let profile = authy::profile_id(&issued.session.identity).unwrap();
        store
            .run("edit", |tx| {
                authy::document().mutate(
                    tx,
                    "boot:persist",
                    &issued.session.identity,
                    &edit_intent(1, &profile, "Persisted", "kept", 1),
                )
            })
            .unwrap();
        (issued.bearer, issued.session.identity, profile)
    };
    {
        let mut store = snap_sqlite::Sqlite::open(&path).unwrap();
        for table in all_tables() {
            store.load(table).unwrap();
        }
        let crypto = Fake::default();
        let account = current(&mut store, &crypto, &bearer, 8);
        assert_eq!(account.identity, identity_id);
        assert_eq!(account.profile, profile);
        let snapshot = store
            .run("read", |tx| {
                authy::document().read(tx, &profile, Some(&identity_id))
            })
            .unwrap()
            .value;
        assert_eq!(snapshot.value, json!({"name": "Persisted", "bio": "kept"}));
        // Second login after restart still maps to the same profile.
        let mut crypto = Fake::default();
        let identity = Identity::default();
        let second = store
            .run("login", |tx| {
                identity.login(tx, &mut crypto, "persist@example.com", "password1", 9)
            })
            .unwrap()
            .value;
        assert_eq!(second.session.identity, identity_id);
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn profile_id_derivation_is_stable_grouped_and_hex_validated() {
    let identity = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(
        authy::profile_id(identity).unwrap(),
        "01234567-89ab-cdef-0123-456789abcdef"
    );
    // Same 32-hex prefix collides by design; different suffixes share a profile.
    // A sibling differing inside the first 32 hex chars maps elsewhere.
    let sibling = "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let twin = format!("{}{}", &identity[..32], "f".repeat(32));
    assert_eq!(
        authy::profile_id(&twin).unwrap(),
        authy::profile_id(identity).unwrap()
    );
    assert_ne!(
        authy::profile_id(sibling).unwrap(),
        authy::profile_id(identity).unwrap()
    );
    // Malformed identities are rejected, never mapped.
    assert!(authy::profile_id("").is_none());
    assert!(authy::profile_id("not-hex").is_none());
    assert!(authy::profile_id(&"0".repeat(63)).is_none());
    assert!(authy::profile_id(&"0".repeat(65)).is_none());
    assert!(authy::profile_id(&"z".repeat(64)).is_none());
}

#[test]
fn authy_migration_applies_cleanly() {
    let parsed: snap_store::migration::Migration = toml::from_str(authy::MIGRATION).unwrap();
    assert_eq!(parsed.id, "0001_authy");
    assert_eq!(parsed.changes.len(), 1);
    let mut store = snap_sqlite::Sqlite::memory(&[parsed]).unwrap();
    for table in authy::TABLES {
        store.load(table).unwrap();
    }
    assert!(store.catalog().table("authy.accounts").is_ok());
    // Unique profile index exists in the declared catalog.
    let table = store.catalog().table("authy.accounts").unwrap();
    assert!(
        table
            .indexes
            .iter()
            .any(|i| i.name == "profile" && i.unique)
    );
}

#[test]
fn missing_metadata_reports_not_found_not_a_profile_leak() {
    let (mut store, mut crypto) = store_loaded();
    let alice = enroll(&mut store, &mut crypto, "leak@example.com", "password1", 0);
    // Delete the metadata row out-of-band of Document to simulate a partial
    // host state: current reports NotFound without revealing profile data.
    store
        .run("delete-meta", |tx| {
            tx.delete(
                authy::TABLES[0],
                &[Value::Text(alice.session.identity.clone())],
            )
            .map(|_| ())
        })
        .unwrap();
    assert!(matches!(
        store.run("gone", |tx| authy::current(tx, &crypto, &alice.bearer, 1)),
        Err(StoreError::NotFound)
    ));
}
