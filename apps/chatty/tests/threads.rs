use serde_json::json;
use snap_document::Manifest;
use snap_store::{Error, Store};

const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";
fn fixture() -> Store<snap_sqlite::Sqlite> {
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
        chatty::MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(chatty::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    store
        .run("create", |tx| {
            chatty::create(
                tx,
                "alice",
                &chatty::Create {
                    id: ID.into(),
                    title: "New thread".into(),
                    created: 1,
                },
            )
        })
        .unwrap();
    store
}

#[test]
fn messages_use_guarded_document_mutations_and_keep_verified_sender() {
    let mut store = fixture();
    let args = json!({"id":"message-1","message":"Hello","created":2});
    assert!(
        store
            .run("denied", |tx| chatty::mutate(
                tx,
                "bob",
                ID,
                "send",
                args.clone()
            ))
            .is_err()
    );
    for _ in 0..2 {
        store
            .run("send", |tx| {
                chatty::mutate(tx, "alice", ID, "send", args.clone())
            })
            .unwrap();
    }
    let doc = store
        .inspect("read", |tx| chatty::document().read(tx, ID, Some("alice")))
        .unwrap();
    let c: chatty::Conversation = serde_json::from_value(doc.value).unwrap();
    assert_eq!(c.turns.len(), 1);
    assert_eq!(c.turns[0].sender, "alice");
    assert_eq!(c.turns[0].status, "complete");
    assert!(c.active_turn.is_empty());
    assert!(
        store
            .run("conflicting duplicate", |tx| chatty::mutate(
                tx,
                "alice",
                ID,
                "send",
                json!({"id":"message-1","message":"Different","created":3})
            ))
            .is_err()
    );
}

#[test]
fn composed_mutations_rollback_and_deletion_retains_the_conversation() {
    let mut store = fixture();
    assert!(
        store
            .run("rollback", |tx| {
                chatty::mutate(tx, "alice", ID, "rename", json!({"title":"Changed"}))?;
                Err::<(), _>(Error::Constraint)
            })
            .is_err()
    );
    assert_eq!(
        store
            .inspect("read", |tx| chatty::document().read(tx, ID, Some("alice")))
            .unwrap()
            .value["title"],
        "New thread"
    );
    store
        .run("delete", |tx| {
            chatty::mutate(tx, "alice", ID, "document.delete", serde_json::Value::Null)
        })
        .unwrap();
    let manifest = store
        .inspect("manifest", |tx| {
            chatty::document().manifest(tx, "test:1", "alice", &Manifest::default())
        })
        .unwrap();
    assert!(manifest.documents.is_empty());
    assert!(
        store
            .inspect("cleanup retains data", |tx| chatty::document().read(
                tx,
                ID,
                Some("alice")
            ))
            .is_ok()
    );
}
