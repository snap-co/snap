use snap_access::{
    Access, Actor, Audience, ChangeSet, GrantChange, KindDefinition, Resource, Role,
};
use snap_document::{
    Definition, Intent, Manifest, Mutation, Registry, Snapshot,
    server::{Document, TABLES},
};
use snap_store::{Error as StoreError, Value};
use snap_transport::json;

const LIFETIME: &str = "boot:1";
const OTHER_LIFETIME: &str = "boot:2";

fn manifest_omits_only_snapshots_matching_every_holding_field<B: snap_store::Backend>(
    mut store: snap_store::Store<B>,
) {
    for table in snap_access::TABLES
        .iter()
        .chain(TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
    {
        store.load(table).unwrap();
    }
    let doc = document();
    let id = uuid(1);
    create_doc(&mut store, &doc, &id, "alice", 5);
    let current = store
        .inspect("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap();
    let holding = snap_document::Holding {
        document: id.clone(),
        version: current.version.clone(),
        revision: current.revision,
        digest: snap_document::digest(&current),
    };
    let mut manifest = Manifest {
        holdings: vec![holding.clone()],
        pending: vec![],
    };
    let unchanged = store
        .inspect("manifest", |tx| {
            doc.access_guard()
                .manifest(tx, LIFETIME, "alice", &manifest)
        })
        .unwrap();
    assert!(unchanged.documents.is_empty());
    assert_eq!(unchanged.unchanged, vec![holding]);
    manifest.holdings[0].digest = "stale".into();
    let stale = store
        .inspect("manifest", |tx| {
            doc.access_guard()
                .manifest(tx, LIFETIME, "alice", &manifest)
        })
        .unwrap();
    assert_eq!(stale.documents, vec![current]);
    assert!(stale.unchanged.is_empty());
}

mod memory {
    use super::*;
    #[test]
    fn manifest_omits_only_snapshots_matching_every_holding_field() {
        let catalog = migrations()
            .iter()
            .try_fold(snap_store::Catalog::default(), |catalog, migration| {
                migration.apply(&catalog)
            })
            .unwrap();
        let backend = snap_platform_tests::memory::Memory::new(catalog.clone()).unwrap();
        super::manifest_omits_only_snapshots_matching_every_holding_field(
            snap_store::Store::new(catalog, backend).unwrap(),
        );
    }
}

mod sqlite_memory {
    use super::*;
    #[test]
    fn manifest_omits_only_snapshots_matching_every_holding_field() {
        super::manifest_omits_only_snapshots_matching_every_holding_field(
            snap_store_sqlite::Sqlite::memory(&migrations()).unwrap(),
        );
    }
}

mod sqlite_file {
    use super::*;
    #[test]
    fn manifest_omits_only_snapshots_matching_every_holding_field() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.sqlite");
        snap_store_sqlite::migrate(&path, &migrations()).unwrap();
        super::manifest_omits_only_snapshots_matching_every_holding_field(
            snap_store_sqlite::Sqlite::open(&path).unwrap(),
        );
    }
}

fn access_migration() -> snap_store::migration::Migration {
    toml::from_str(snap_access::MIGRATION).unwrap()
}

fn doc_migration() -> snap_store::migration::Migration {
    toml::from_str(snap_document::server::MIGRATION).unwrap()
}

fn notes_migration() -> snap_store::migration::Migration {
    toml::from_str(
        r#"
id = "0002_notes"

[[changes]]
action = "create_table"
[changes.table]
name = "test.notes"
primary = ["id"]
columns = [{ name = "id", kind = "text" }, { name = "body", kind = "text" }]
"#,
    )
    .unwrap()
}

fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut all = vec![
        access_migration(),
        doc_migration(),
        toml::from_str(snap_store::resource::MIGRATION).unwrap(),
    ];
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all
}

fn migrations_with_notes() -> Vec<snap_store::migration::Migration> {
    let mut all = vec![
        access_migration(),
        doc_migration(),
        notes_migration(),
        toml::from_str(snap_store::resource::MIGRATION).unwrap(),
    ];
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all
}

fn store_loaded() -> Store {
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
    {
        store.load(table).unwrap();
    }
    store
}

#[test]
fn admission_guards_read_related_resident_state_and_preserve_store_misses() {
    let doc = Document::new(
        Registry::new(vec![Definition {
            kind: "counter".into(),
            version: "1".into(),
            validate: |v| v.is_i64(),
            mutations: vec![Mutation {
                name: "ready".into(),
                minimum: Role::Owner,
                guard: Some(|tx, snapshot, _, _, _| {
                    Ok(tx
                        .get("test.notes", &[snapshot.id.clone().into()])?
                        .is_some())
                }),
                apply: |_, _, _| Ok(json!(1)),
            }],
        }])
        .unwrap(),
    );
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations_with_notes()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
    {
        store.load(table).unwrap();
    }
    let id = uuid(99);
    create_doc(&mut store, &doc, &id, "alice", 0);
    let operation = intent(1, &id, "ready", 0);
    assert!(matches!(
        store.inspect("cold guard", |tx| doc
            .access_guard()
            .admit(tx, "alice", &operation)),
        Err(StoreError::Miss(_))
    ));
    store.load("test.notes").unwrap();
    assert!(matches!(
        store
            .inspect("missing prerequisite", |tx| doc
                .access_guard()
                .admit(tx, "alice", &operation))
            .unwrap(),
        Err(snap_document::Error::Denied)
    ));
    store
        .run("prerequisite and mutation", |tx| {
            tx.insert(
                "test.notes",
                [
                    ("id".into(), id.clone().into()),
                    ("body".into(), "ready".into()),
                ]
                .into_iter()
                .collect(),
            )?;
            let admitted = doc
                .access_guard()
                .admit(tx, "alice", &operation)?
                .map_err(|_| StoreError::Invalid)?;
            doc.execute(tx, admitted)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .inspect("read", |tx| doc.read(tx, &id, Some("alice")))
            .unwrap()
            .value,
        json!(1)
    );
}

type Store = snap_store::Store<snap_store_sqlite::Sqlite>;

fn dispatch_document(
    store: &mut Store,
    lifetime: &str,
    actor: &str,
    name: &str,
    input: snap_transport::Value,
) -> snap_transport::Outcome {
    use snap_transport::operation::{Context, Runtime};
    let mut runtime = Runtime::default();
    for definition in snap_document::operations::definitions(std::sync::Arc::new(document())) {
        runtime.register(definition).unwrap();
    }
    let selection = runtime.definitions().resolve(name)?;
    runtime.enqueue(
        (),
        snap_transport::Invocation {
            id: 1,
            operation: name.into(),
            input,
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

fn access() -> Access {
    Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap()
}

fn registry() -> Registry {
    Registry::new(vec![Definition {
        kind: "counter".into(),
        version: "1".into(),
        validate: |value| value.as_i64().is_some(),
        mutations: vec![
            Mutation {
                name: "add".into(),
                minimum: Role::Editor,
                guard: None,
                apply: |value, args, _| {
                    Ok(json!(
                        value.as_i64().unwrap()
                            + args.as_i64().ok_or(snap_document::Error::Invalid)?
                    ))
                },
            },
            Mutation {
                name: "touch".into(),
                minimum: Role::Viewer,
                guard: Some(|_, _, _, actor, _| Ok(actor == "alice")),
                apply: |value, _, _| Ok(value.clone()),
            },
        ],
    }])
    .unwrap()
}

fn document() -> Document {
    Document::new(registry())
}

fn uuid(n: u32) -> String {
    format!("018f3c4b-6d2a-7000-8000-{:012x}", n)
}

fn snapshot(id: &str, value: i64, revision: u64) -> Snapshot {
    Snapshot {
        id: id.into(),
        kind: "counter".into(),
        version: "1".into(),
        revision,
        value: json!(value),
    }
}

fn intent(id: u64, document: &str, mutation: &str, args: i64) -> Intent {
    Intent {
        id,
        document: document.into(),
        version: "1".into(),
        mutation: mutation.into(),
        args: json!(args),
    }
}

fn create_doc<B: snap_store::Backend>(
    store: &mut snap_store::Store<B>,
    doc: &Document,
    id: &str,
    owner: &str,
    value: i64,
) {
    store
        .run("create", |tx| {
            doc.create(tx, &snapshot(id, value, 1), Audience::Restricted, owner)
        })
        .unwrap();
}

#[test]
fn application_replacement_and_removal_preserve_revision_rollback_and_lifecycle() {
    let mut store = store_loaded();
    let doc = document();
    let id = uuid(91);
    create_doc(&mut store, &doc, &id, "alice", 1);
    // Replacement/removal own schema, revision, rollback and lifecycle. Owner
    // rejection belongs to composed dispatch, not this persistence interface.
    assert_eq!(
        store
            .run("invalid shape", |tx| doc.replace(
                tx,
                &id,
                "alice",
                json!("bad")
            ))
            .unwrap_err(),
        StoreError::Invalid
    );
    assert_eq!(
        store
            .run("rollback", |tx| {
                doc.replace(tx, &id, "alice", json!(4))?;
                Err::<(), _>(StoreError::Constraint)
            })
            .unwrap_err(),
        StoreError::Constraint
    );
    assert_eq!(
        store
            .run("read", |tx| doc.read(tx, &id, Some("alice")))
            .unwrap()
            .value,
        snapshot(&id, 1, 1)
    );
    assert_eq!(
        store
            .run("replace", |tx| doc.replace(tx, &id, "alice", json!(4)))
            .unwrap()
            .value,
        snapshot(&id, 4, 2)
    );
    store
        .run("remove", |tx| doc.remove(tx, &id, "alice"))
        .unwrap();
    assert_eq!(
        store
            .run("retained for cleanup", |tx| doc.read(
                tx,
                &id,
                Some("alice")
            ))
            .unwrap()
            .value,
        snapshot(&id, 4, 2)
    );
    assert!(
        store
            .run("excluded from normal loading", |tx| doc
                .access_guard()
                .manifest(tx, LIFETIME, "alice", &Manifest::default()))
            .unwrap()
            .value
            .documents
            .is_empty()
    );
}

fn grant(store: &mut Store, id: &str, identity: &str, role: Role) {
    store
        .run("grant", |tx| {
            let mut changes = ChangeSet::new(Actor::system());
            changes.grants.push(GrantChange {
                resource: Resource::new("document", id).unwrap(),
                identity: identity.into(),
                role: Some(role),
            });
            access().change(tx, &changes).map(|_| ())
        })
        .unwrap();
}

#[test]
fn create_and_resident_read_round_trip() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(1);
    create_doc(&mut store, &doc, &id, "alice", 0);
    grant(&mut store, &id, "bob", Role::Viewer);
    let seen = store
        .run("read", |tx| doc.read(tx, &id, Some("bob")))
        .unwrap()
        .value;
    assert_eq!(seen.value, json!(0));
    assert_eq!(seen.revision, 1);
}

#[test]
fn create_rejects_duplicates_conflicts_and_invalid_registry() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(10);
    create_doc(&mut store, &doc, &id, "alice", 0);
    // Duplicate id, even with identical payload, is misuse.
    let duplicate = store.run("dup", |tx| {
        doc.create(tx, &snapshot(&id, 0, 1), Audience::Restricted, "alice")
    });
    assert!(matches!(duplicate, Err(StoreError::Invalid)));
    // Unknown app kind.
    let unknown = store.run("unknown", |tx| {
        doc.create(
            tx,
            &Snapshot {
                id: uuid(11),
                kind: "missing".into(),
                version: "1".into(),
                revision: 1,
                value: json!(0),
            },
            Audience::Restricted,
            "alice",
        )
    });
    assert!(matches!(unknown, Err(StoreError::Invalid)));
    // Version mismatch.
    let mismatch = store.run("mismatch", |tx| {
        doc.create(
            tx,
            &Snapshot {
                id: uuid(12),
                kind: "counter".into(),
                version: "9".into(),
                revision: 1,
                value: json!(0),
            },
            Audience::Restricted,
            "alice",
        )
    });
    assert!(matches!(mismatch, Err(StoreError::Invalid)));
    // Zero revision and malformed id.
    assert!(matches!(
        store.run("zero", |tx| doc.create(
            tx,
            &snapshot(&uuid(13), 0, 0),
            Audience::Restricted,
            "alice"
        )),
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store.run("bad-id", |tx| doc.create(
            tx,
            &Snapshot {
                id: "not-a-uuid".into(),
                kind: "counter".into(),
                version: "1".into(),
                revision: 1,
                value: json!(0),
            },
            Audience::Restricted,
            "alice"
        )),
        Err(StoreError::Invalid)
    ));
    // Storage-unsafe revision.
    assert!(matches!(
        store.run("huge", |tx| doc.create(
            tx,
            &Snapshot {
                id: uuid(14),
                kind: "counter".into(),
                version: "1".into(),
                revision: u64::MAX,
                value: json!(0),
            },
            Audience::Restricted,
            "alice"
        )),
        Err(StoreError::Invalid)
    ));
    // Empty owner.
    assert!(matches!(
        store.run("owner", |tx| doc.create(
            tx,
            &snapshot(&uuid(15), 0, 1),
            Audience::Restricted,
            ""
        )),
        Err(StoreError::Invalid)
    ));
    // Failed creates staged nothing: only the first document exists.
    let listed = store
        .run("list", |tx| {
            doc.access_guard()
                .manifest(tx, LIFETIME, "alice", &Manifest::default())
        })
        .unwrap()
        .value;
    assert_eq!(listed.documents.len(), 1);
    assert_eq!(listed.documents[0].id, id);
}

#[test]
fn guards_and_minimum_roles_deny_without_document_writes() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(20);
    create_doc(&mut store, &doc, &id, "alice", 5);
    grant(&mut store, &id, "bob", Role::Viewer);
    grant(&mut store, &id, "carol", Role::Editor);

    // Viewer cannot run Editor-gated "add" (pre-change minimum).
    let denied = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.mutate",
        json!(intent(1, &id, "add", 1)),
    );
    assert_eq!(
        denied,
        Err(snap_transport::Error::Application(json!(
            snap_document::Error::Denied
        )))
    );

    // Guard denies bob on "touch" even though Viewer satisfies the minimum;
    // alice passes the same guard.
    let guarded = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.mutate",
        json!(Intent {
            id: 2,
            document: id.clone(),
            version: "1".into(),
            mutation: "touch".into(),
            args: json!(0),
        }),
    );
    assert_eq!(
        guarded,
        Err(snap_transport::Error::Application(json!(
            snap_document::Error::Denied
        )))
    );
    let allowed = dispatch_document(
        &mut store,
        OTHER_LIFETIME,
        "alice",
        "document.mutate",
        json!(Intent {
            id: 1,
            document: id.clone(),
            version: "1".into(),
            mutation: "touch".into(),
            args: json!(0),
        }),
    );
    assert!(allowed.is_ok());

    // Neither denial advanced the document: only the successful
    // guard-passing "touch" committed (a value-identical apply still advances
    // the revision per Registry::apply); value still 5.
    let after = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(after.revision, 2);
    assert_eq!(after.value, json!(5));

    // An Editor succeeds and advances exactly once more.
    let edited = dispatch_document(
        &mut store,
        "boot:3",
        "carol",
        "document.mutate",
        json!(intent(1, &id, "add", 2)),
    )
    .unwrap();
    let snap_document::ServerMessage::Completed(completion) =
        serde_json::from_value(edited).unwrap()
    else {
        panic!("mutation completion")
    };
    let next = completion.result.unwrap().unwrap();
    assert_eq!(next.value, json!(7));
    assert_eq!(next.revision, 3);
}

#[test]
fn denied_mutations_leak_no_document_data() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(30);
    create_doc(&mut store, &doc, &id, "alice", 42);
    // Bob has no grant at all.
    let denied = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.mutate",
        json!(intent(1, &id, "add", 1)),
    );
    assert_eq!(
        denied,
        Err(snap_transport::Error::Application(json!(
            snap_document::Error::Denied
        )))
    );
    // Manifest for bob contains no documents.
    let listed = store
        .run("manifest", |tx| {
            doc.access_guard()
                .manifest(tx, LIFETIME, "bob", &Manifest::default())
        })
        .unwrap()
        .value;
    assert!(listed.documents.is_empty());
}

#[test]
fn receipts_dedup_exact_intents_and_reject_conflicts() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(40);
    create_doc(&mut store, &doc, &id, "alice", 0);

    let first = store
        .run("first", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 3))
        })
        .unwrap()
        .value;
    assert!(!first.replayed);
    assert_eq!(
        first
            .completion
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .value,
        json!(3)
    );

    // Exact retry replays without re-executing: same completion, no fan-out.
    let replay = store
        .run("replay", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 3))
        })
        .unwrap()
        .value;
    assert!(replay.replayed);
    assert!(replay.replication.is_none());
    assert_eq!(replay.completion, first.completion);

    // Revision advanced only once.
    let current = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(current.revision, 2);

    // Same lifetime+id with different args is a conflict.
    let conflict = store.run("conflict", |tx| {
        doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 4))
    });
    assert!(matches!(conflict, Err(StoreError::Invalid)));
    // Different actor on the same key is also a conflict.
    grant(&mut store, &id, "bob", Role::Editor);
    let actor_conflict = store.run("actor", |tx| {
        doc.mutate(tx, LIFETIME, "bob", &intent(1, &id, "add", 3))
    });
    assert!(matches!(actor_conflict, Err(StoreError::Invalid)));
    // Same intent id on a different lifetime is independent work.
    let other = store
        .run("other", |tx| {
            doc.mutate(tx, OTHER_LIFETIME, "alice", &intent(1, &id, "add", 3))
        })
        .unwrap()
        .value;
    assert!(!other.replayed);
    let advanced = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(advanced.revision, 3);
    assert_eq!(advanced.value, json!(6));
}

#[test]
fn revoked_access_suppresses_successful_recovery_payloads_but_preserves_rejections() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(50);
    create_doc(&mut store, &doc, &id, "alice", 0);
    grant(&mut store, &id, "bob", Role::Editor);
    let committed = store
        .run("commit", |tx| {
            doc.mutate(tx, LIFETIME, "bob", &intent(7, &id, "add", 5))
        })
        .unwrap()
        .value;
    assert!(committed.completion.result.is_ok());

    // A deterministic rejection is durably recorded too. Visibility loss must
    // not recategorize it as a committed write with a forbidden payload.
    let mut rejected_intent = intent(8, &id, "add", 0);
    rejected_intent.args = json!("invalid amount");
    let rejected = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.mutate",
        json!(rejected_intent),
    )
    .unwrap();
    let snap_document::ServerMessage::Completed(rejection) =
        serde_json::from_value(rejected).unwrap()
    else {
        panic!("rejected completion")
    };
    assert_eq!(rejection.result, Err(snap_document::Error::Invalid));

    // Revoke bob entirely.
    store
        .run("revoke", |tx| {
            let mut changes = ChangeSet::new(Actor::system());
            changes.grants.push(GrantChange {
                resource: Resource::new("document", &id).unwrap(),
                identity: "bob".into(),
                role: None,
            });
            access().change(tx, &changes).map(|_| ())
        })
        .unwrap();

    // Retry of the same receipt suppresses the snapshot but keeps the id.
    let reread = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.mutate",
        json!(intent(7, &id, "add", 5)),
    )
    .unwrap();
    let snap_document::ServerMessage::Completed(completion) =
        serde_json::from_value(reread).unwrap()
    else {
        panic!("replay completion")
    };
    assert_eq!(completion.id, 7);
    assert!(matches!(completion.result, Ok(None)));

    let replayed = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.mutate",
        json!(rejected_intent),
    )
    .unwrap();
    let snap_document::ServerMessage::Completed(replayed_rejection) =
        serde_json::from_value(replayed).unwrap()
    else {
        panic!("rejected replay")
    };
    assert_eq!(replayed_rejection, rejection);

    // Manifest recovery for the same pending intent is suppressed the same way,
    // while the document itself is absent from bob's replacement.
    let recovered = dispatch_document(
        &mut store,
        LIFETIME,
        "bob",
        "document.manifest",
        json!(Manifest {
            holdings: vec![],
            pending: vec![intent(7, &id, "add", 5), rejected_intent],
        }),
    )
    .unwrap();
    let snap_document::ServerMessage::Manifest(recovered) =
        serde_json::from_value(recovered).unwrap()
    else {
        panic!("recovery manifest")
    };
    assert!(recovered.documents.is_empty());
    assert_eq!(recovered.completed.len(), 2);
    assert!(matches!(recovered.completed[0].result, Ok(None)));
    assert_eq!(recovered.completed[1], rejection);

    // Alice still sees the committed value.
    let kept = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(kept.value, json!(5));
}

#[test]
fn manifest_returns_full_replacement_and_recovers_pending() {
    let doc = document();
    let mut store = store_loaded();
    let first = uuid(60);
    let second = uuid(61);
    create_doc(&mut store, &doc, &first, "alice", 1);
    create_doc(&mut store, &doc, &second, "alice", 2);
    let committed = store
        .run("mutate", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &first, "add", 10))
        })
        .unwrap()
        .value;
    assert_eq!(
        committed
            .completion
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .value,
        json!(11)
    );
    // Holdings are ignored: the server always answers with the full
    // authorized set, not a delta.
    let state = store
        .run("manifest", |tx| {
            doc.access_guard()
                .manifest(tx, LIFETIME, "alice", &Manifest::default())
        })
        .unwrap()
        .value;
    assert_eq!(state.documents.len(), 2);
    assert!(state.completed.is_empty());
    // Pending intents with receipts recover; unknown pending stays pending
    // (no completion returned).
    let recovery = store
        .run("recover", |tx| {
            doc.access_guard().manifest(
                tx,
                LIFETIME,
                "alice",
                &Manifest {
                    holdings: vec![],
                    pending: vec![intent(1, &first, "add", 10), intent(9, &second, "add", 1)],
                },
            )
        })
        .unwrap()
        .value;
    assert_eq!(recovery.documents.len(), 2);
    assert_eq!(recovery.completed.len(), 1);
    assert_eq!(recovery.completed[0].id, 1);
}

#[test]
fn mutations_serialize_onto_latest_state_without_stale_rejection() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(70);
    create_doc(&mut store, &doc, &id, "alice", 0);
    for n in 1..=3 {
        let result = store
            .run("seq", |tx| {
                doc.mutate(tx, LIFETIME, "alice", &intent(n, &id, "add", 1))
            })
            .unwrap()
            .value;
        assert!(!result.replayed);
    }
    let current = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(current.revision, 4);
    assert_eq!(current.value, json!(3));
    // Replication chains base to result across the serialized writes.
    let later = store
        .run("later", |tx| {
            doc.mutate(tx, OTHER_LIFETIME, "alice", &intent(1, &id, "add", 10))
        })
        .unwrap()
        .value;
    let replication = later.replication.unwrap();
    assert_eq!(replication.base_revision, 4);
    assert_eq!(replication.revision, 5);
    assert!(!replication.base_digest.is_empty());
    assert!(!replication.result_digest.is_empty());
    assert_ne!(replication.base_digest, replication.result_digest);
}

#[test]
fn access_and_document_writes_roll_back_together() {
    let doc = document();
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations_with_notes()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
        .chain(["test.notes"].iter())
    {
        store.load(table).unwrap();
    }
    let id = uuid(80);
    store
        .run("create", |tx| {
            doc.create(tx, &snapshot(&id, 0, 1), Audience::Restricted, "alice")?;
            let mut row = snap_store::Row::new();
            row.insert("id".into(), Value::Text("note-1".into()));
            row.insert("body".into(), Value::Text("hello".into()));
            tx.insert("test.notes", row)?;
            Ok(())
        })
        .unwrap();
    // A caller failure after both writes discards both.
    let failed = store.run("abort", |tx| {
        doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 1))?;
        let mut row = snap_store::Row::new();
        row.insert("id".into(), Value::Text("note-2".into()));
        row.insert("body".into(), Value::Text("dropped".into()));
        tx.insert("test.notes", row)?;
        Err::<(), _>(StoreError::Unavailable)
    });
    assert!(matches!(failed, Err(StoreError::Unavailable)));
    let rolled = store
        .run("read", |tx| {
            Ok((
                doc.read(tx, &id, Some("alice"))?,
                tx.get("test.notes", &[Value::Text("note-2".into())])?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(rolled.0.revision, 1);
    assert!(rolled.1.is_none());
}

#[test]
fn cold_tables_miss_and_stage_nothing() {
    let doc = document();
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    let id = uuid(90);
    assert!(matches!(
        store.run("cold-create", |tx| doc.create(
            tx,
            &snapshot(&id, 0, 1),
            Audience::Restricted,
            "alice"
        )),
        Err(StoreError::Miss(_))
    ));
    assert!(matches!(
        store.run("cold-mutate", |tx| doc.mutate(
            tx,
            LIFETIME,
            "alice",
            &intent(1, &id, "add", 1)
        )),
        Err(StoreError::Miss(_))
    ));
    assert!(matches!(
        store.run("cold-manifest", |tx| doc.access_guard().manifest(
            tx,
            LIFETIME,
            "alice",
            &Manifest::default()
        )),
        Err(StoreError::Miss(_))
    ));
    assert!(matches!(
        store.run("cold-expire", |tx| doc.expire(tx, LIFETIME)),
        Err(StoreError::Miss(_))
    ));
    // A miss poisons the whole attempt even when caught: no partial commit.
    let poisoned = store.run("poison", |tx| {
        let _ = doc
            .access_guard()
            .manifest(tx, LIFETIME, "alice", &Manifest::default());
        Ok(())
    });
    assert!(matches!(poisoned, Err(StoreError::Miss(_))));
    for table in snap_access::TABLES
        .iter()
        .chain(TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
    {
        store.load(table).unwrap();
    }
    // Nothing from the cold attempts survived.
    let empty = store
        .run("empty", |tx| {
            doc.access_guard()
                .manifest(tx, LIFETIME, "alice", &Manifest::default())
        })
        .unwrap()
        .value;
    assert!(empty.documents.is_empty());
}

#[test]
fn expire_removes_receipts_only() {
    let doc = document();
    let mut store = store_loaded();
    let id = uuid(100);
    create_doc(&mut store, &doc, &id, "alice", 0);
    let first = store
        .run("first", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 2))
        })
        .unwrap()
        .value;
    assert!(!first.replayed);
    store.run("expire", |tx| doc.expire(tx, LIFETIME)).unwrap();
    // The document survives expiry; the receipt does not, so the same intent
    // executes anew instead of replaying.
    let kept = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(kept.value, json!(2));
    let again = store
        .run("again", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 2))
        })
        .unwrap()
        .value;
    assert!(!again.replayed);
    let advanced = store
        .run("read", |tx| doc.read(tx, &id, Some("alice")))
        .unwrap()
        .value;
    assert_eq!(advanced.value, json!(4));
    // Expiring an unknown lifetime is a no-op success.
    store
        .run("noop", |tx| doc.expire(tx, "boot:unknown"))
        .unwrap();
}

#[test]
fn receipts_survive_restart_and_manifest_recovers() {
    let path = std::path::PathBuf::from(format!(
        "/tmp/opencode/snap-document-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    snap_store_sqlite::migrate(&path, &migrations()).unwrap();
    let id = uuid(110);
    {
        let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
        for table in snap_access::TABLES
            .iter()
            .chain(TABLES.iter())
            .chain(core::iter::once(&snap_store::resource::TABLE))
        {
            store.load(table).unwrap();
        }
        let doc = document();
        store
            .run("create", |tx| {
                doc.create(tx, &snapshot(&id, 0, 1), Audience::Restricted, "alice")
            })
            .unwrap();
        store
            .run("mutate", |tx| {
                doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 6))
            })
            .unwrap();
    }
    {
        let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
        for table in snap_access::TABLES
            .iter()
            .chain(TABLES.iter())
            .chain(core::iter::once(&snap_store::resource::TABLE))
        {
            store.load(table).unwrap();
        }
        let doc = document();
        // Same lifetime+intent replays durably without doubling the write.
        let replay = store
            .run("replay", |tx| {
                doc.mutate(tx, LIFETIME, "alice", &intent(1, &id, "add", 6))
            })
            .unwrap()
            .value;
        assert!(replay.replayed);
        assert_eq!(
            replay
                .completion
                .result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .value,
            json!(6)
        );
        let recovery = store
            .run("recover", |tx| {
                doc.access_guard().manifest(
                    tx,
                    LIFETIME,
                    "alice",
                    &Manifest {
                        holdings: vec![],
                        pending: vec![intent(1, &id, "add", 6)],
                    },
                )
            })
            .unwrap()
            .value;
        assert_eq!(recovery.completed.len(), 1);
        assert_eq!(recovery.documents[0].value, json!(6));
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn unknown_documents_and_malformed_intents_report_cleanly() {
    let doc = document();
    let mut store = store_loaded();
    // Missing document is a domain NotFound with a deduplicating receipt.
    let missing = store
        .run("missing", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &uuid(120), "add", 1))
        })
        .unwrap()
        .value;
    assert!(matches!(
        missing.completion.result,
        Err(snap_document::Error::NotFound)
    ));
    let again = store
        .run("again", |tx| {
            doc.mutate(tx, LIFETIME, "alice", &intent(1, &uuid(120), "add", 1))
        })
        .unwrap()
        .value;
    assert!(again.replayed);
    // Malformed intents are misuse without receipts.
    assert!(matches!(
        store.run("zero", |tx| doc.mutate(
            tx,
            LIFETIME,
            "alice",
            &intent(0, &uuid(121), "add", 1)
        )),
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store.run("empty-lifetime", |tx| doc.mutate(
            tx,
            "",
            "alice",
            &intent(1, &uuid(121), "add", 1)
        )),
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store.run("empty-actor", |tx| doc.mutate(
            tx,
            LIFETIME,
            "",
            &intent(1, &uuid(121), "add", 1)
        )),
        Err(StoreError::Invalid)
    ));
    // Unknown mutation name is a domain error with a receipt.
    let id = uuid(122);
    create_doc(&mut store, &doc, &id, "alice", 0);
    let unknown = store
        .run("unknown", |tx| {
            doc.mutate(
                tx,
                LIFETIME,
                "alice",
                &Intent {
                    id: 5,
                    document: id.clone(),
                    version: "1".into(),
                    mutation: "missing".into(),
                    args: json!(0),
                },
            )
        })
        .unwrap()
        .value;
    assert!(matches!(
        unknown.completion.result,
        Err(snap_document::Error::Invalid)
    ));
}

#[test]
fn document_migration_applies_cleanly() {
    let parsed: snap_store::migration::Migration =
        toml::from_str(snap_document::server::MIGRATION).unwrap();
    let mut store = snap_store_sqlite::Sqlite::memory(&[parsed]).unwrap();
    for table in TABLES {
        store.load(table).unwrap();
    }
    assert!(store.catalog().table("document.documents").is_ok());
    assert!(store.catalog().table("document.receipts").is_ok());
    assert!(store.catalog().table("document.lifecycle").is_err());
}

#[test]
fn durable_constraint_rejection_discards_the_mutation_and_its_receipt() {
    let mut store = store_loaded();
    let doc = document();
    let id = uuid(1);
    create_doc(&mut store, &doc, &id, "alice", 0);
    let edit = intent(1, &id, "add", 1);
    let result = store.run("rejected-backend-commit", |tx| {
        let result = doc.mutate(tx, "connection", "alice", &edit)?;
        assert!(result.completion.result.is_ok());
        // Deferred foreign key validation rejects at the real SQLite commit,
        // after the handler has successfully staged both write and receipt.
        tx.insert(
            "access.links",
            [
                ("child".into(), snap_store::Value::from(uuid(99))),
                ("parent".into(), snap_store::Value::from(id.clone())),
            ]
            .into_iter()
            .collect(),
        )?;
        Ok(())
    });
    assert_eq!(result.unwrap_err(), snap_store::Error::Constraint);
    let retry = store
        .run("explicit-new-attempt", |tx| {
            doc.mutate(tx, "connection", "alice", &edit)
        })
        .unwrap()
        .value;
    assert!(!retry.replayed);
    assert_eq!(
        retry.completion.result.unwrap().unwrap().value,
        serde_json::json!(1)
    );
}
