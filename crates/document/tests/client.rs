use serde_json::{Value, json};
use snap_document::client::{Client, Outcome};
use snap_document::{
    ClientMessage, Completion, Error, Intent, Manifest, Reconciliation, Registry, Replication,
    ServerMessage, Snapshot, digest,
};

fn validate_counter(value: &Value) -> bool {
    value
        .get("count")
        .and_then(|count| count.as_i64())
        .is_some()
}

fn inc_apply(state: &Value, _args: &Value, _actor: &str) -> Result<Value, Error> {
    let count = state
        .get("count")
        .and_then(|count| count.as_i64())
        .ok_or(Error::Invalid)?;
    let next = count.checked_add(1).ok_or(Error::Invalid)?;
    Ok(json!({ "count": next }))
}

fn add_apply(state: &Value, args: &Value, _actor: &str) -> Result<Value, Error> {
    let count = state
        .get("count")
        .and_then(|count| count.as_i64())
        .ok_or(Error::Invalid)?;
    let delta = args
        .get("delta")
        .and_then(|delta| delta.as_i64())
        .ok_or(Error::Invalid)?;
    let next = count.checked_add(delta).ok_or(Error::Invalid)?;
    Ok(json!({ "count": next }))
}

fn set_apply(state: &Value, args: &Value, _actor: &str) -> Result<Value, Error> {
    let _ = state
        .get("count")
        .and_then(|count| count.as_i64())
        .ok_or(Error::Invalid)?;
    let value = args
        .get("value")
        .and_then(|value| value.as_i64())
        .ok_or(Error::Invalid)?;
    Ok(json!({ "count": value }))
}

fn capped_apply(state: &Value, _args: &Value, _actor: &str) -> Result<Value, Error> {
    let count = state
        .get("count")
        .and_then(|count| count.as_i64())
        .ok_or(Error::Invalid)?;
    let next = count.checked_add(1).ok_or(Error::Invalid)?;
    if next > 3 {
        return Err(Error::Invalid);
    }
    Ok(json!({ "count": next }))
}

fn registry() -> Registry {
    Registry::new(vec![snap_document::Definition {
        kind: "counter".into(),
        version: "1".into(),
        validate: validate_counter,
        mutations: vec![
            snap_document::Mutation {
                name: "inc".into(),
                minimum: snap_access::Role::Editor,
                apply: inc_apply,
                guard: None,
            },
            snap_document::Mutation {
                name: "add".into(),
                minimum: snap_access::Role::Editor,
                apply: add_apply,
                guard: None,
            },
            snap_document::Mutation {
                name: "set".into(),
                minimum: snap_access::Role::Editor,
                apply: set_apply,
                guard: None,
            },
            snap_document::Mutation {
                name: "inc_capped".into(),
                minimum: snap_access::Role::Editor,
                apply: capped_apply,
                guard: None,
            },
        ],
    }])
    .unwrap()
}

fn snapshot(id: &str, revision: u64, count: i64) -> Snapshot {
    Snapshot {
        id: id.into(),
        kind: "counter".into(),
        version: "1".into(),
        revision,
        value: json!({ "count": count }),
    }
}

fn install(client: &mut Client, registry: &Registry, documents: Vec<Snapshot>) {
    let outcome = client
        .handle(
            registry,
            ServerMessage::Manifest(Reconciliation {
                documents,
                completed: Vec::new(),
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }), "{outcome:?}");
}

#[test]
fn unsolicited_holdings_cannot_open_recovery_or_double_apply_a_committed_pending_write() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id = client
        .enqueue(&registry, "doc-a", "inc", Value::Null)
        .unwrap();
    mutate_message(&mut client);
    client
        .handle(&registry, ServerMessage::Accepted { id })
        .unwrap();
    client.begin_reconnect();
    let committed = snapshot("doc-a", 2, 1);
    assert_eq!(
        client
            .handle(&registry, ServerMessage::Holdings(vec![committed.clone()]))
            .unwrap(),
        Outcome::Deferred
    );
    assert_eq!(client.get("doc-a").unwrap().value, json!({"count": 1}));
    assert!(client.next_submission().is_none());
    client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![committed.clone()],
                completed: vec![Completion {
                    id,
                    document: "doc-a".into(),
                    result: Ok(Some(committed)),
                }],
            }),
        )
        .unwrap();
    assert!(client.pending().is_empty());
    assert_eq!(client.get("doc-a").unwrap().value, json!({"count": 1}));
}

fn mutate_message(client: &mut Client) -> Intent {
    match client.next_submission().expect("expected submission") {
        ClientMessage::Mutate(intent) => intent,
        ClientMessage::Manifest(_) => panic!("expected Mutate, got Manifest"),
    }
}

fn replication_for(
    registry: &Registry,
    base: &Snapshot,
    intent: &Intent,
    actor: &str,
) -> Replication {
    let applied = registry.apply(base, intent, actor).unwrap();
    Replication {
        base_revision: base.revision,
        base_digest: digest(base),
        revision: applied.revision,
        result_digest: digest(&applied),
        intent: intent.clone(),
        actor: actor.into(),
    }
}

#[test]
fn ack_permits_second_while_first_pending() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let rev0 = client.revision();

    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    assert_ne!(id1, id2);
    assert_ne!(id1, 0);
    assert_eq!(client.pending().len(), 2);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 2 }));
    assert!(client.revision() > rev0);

    let first = mutate_message(&mut client);
    assert_eq!(first.id, id1);
    assert_eq!(client.awaiting_ack(), Some(id1));
    // Only one outstanding: second is blocked until the first is accepted.
    assert!(client.next_submission().is_none());

    let outcome = client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();
    assert_eq!(outcome, Outcome::Accepted { id: id1 });
    // Acceptance does not clear optimistic state and does not touch the view.
    assert_eq!(client.pending().len(), 2);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 2 }));

    let second = mutate_message(&mut client);
    assert_eq!(second.id, id2);
    let outcome = client
        .handle(&registry, ServerMessage::Accepted { id: id2 })
        .unwrap();
    assert_eq!(outcome, Outcome::Accepted { id: id2 });
    assert_eq!(client.pending().len(), 2);
}

#[test]
fn completion_applies_once_and_retains_later() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);

    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "add", json!({ "delta": 5 }))
        .unwrap();
    let _ = mutate_message(&mut client);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();
    let _ = mutate_message(&mut client);
    client
        .handle(&registry, ServerMessage::Accepted { id: id2 })
        .unwrap();

    // Server commits the first intent: base rev1 count0 -> rev2 count1.
    let base = snapshot("doc-a", 1, 0);
    let intent1 = client
        .pending()
        .iter()
        .find(|intent| intent.id == id1)
        .unwrap()
        .clone();
    let committed = registry.apply(&base, &intent1, "alice").unwrap();
    assert_eq!(committed.revision, 2);
    assert_eq!(committed.value, json!({ "count": 1 }));

    let outcome = client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: id1,
                document: "doc-a".into(),
                result: Ok(Some(committed.clone())),
            }),
        )
        .unwrap();
    assert_eq!(outcome, Outcome::Completed { id: id1 });

    // Exactly one entry removed; the later intent is retained and rebased.
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, id2);
    assert_eq!(client.authoritative()["doc-a"], committed);
    // View is authoritative (rev2 count1) plus the retained add(+5) -> rev3 count6.
    // No double application of the completed intent.
    assert_eq!(client.authoritative()["doc-a"].revision, 2);
    assert_eq!(client.view()["doc-a"].revision, 3);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 6 }));
}

#[test]
fn remote_replication_rebases_pending() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 10)]);

    let local = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 11 }));

    // Remote actor advances rev1 count10 -> rev2 count20 via set.
    let remote_intent = Intent {
        id: 77,
        document: "doc-a".into(),
        version: "1".into(),
        mutation: "set".into(),
        args: json!({ "value": 20 }),
    };
    let base = snapshot("doc-a", 1, 10);
    let replication = replication_for(&registry, &base, &remote_intent, "bob");
    let outcome = client
        .handle(&registry, ServerMessage::Replication(replication))
        .unwrap();
    assert!(matches!(outcome, Outcome::Replicated { revision: 2, .. }));
    // Authoritative moved to the remote result; the local pending rebased on top.
    assert_eq!(
        client.authoritative()["doc-a"].value,
        json!({ "count": 20 })
    );
    assert_eq!(client.authoritative()["doc-a"].revision, 2);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 21 }));
    assert_eq!(client.view()["doc-a"].revision, 3);
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, local);
}

#[test]
fn mismatch_reports_and_requests_manifest_recovery() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let auth_before = client.authoritative()["doc-a"].clone();
    let view_before = client.view()["doc-a"].clone();

    // Wrong base digest: must not apply.
    let bad = Replication {
        intent: Intent {
            id: 9,
            document: "doc-a".into(),
            version: "1".into(),
            mutation: "inc".into(),
            args: json!({}),
        },
        actor: "bob".into(),
        base_revision: 1,
        base_digest: "deadbeef".into(),
        revision: 2,
        result_digest: "deadbeef".into(),
    };
    let outcome = client
        .handle(&registry, ServerMessage::Replication(bad))
        .unwrap();
    match outcome {
        Outcome::NeedManifest { manifest, error } => {
            assert!(matches!(error, Error::Diverged(_)));
            assert_eq!(manifest.pending.len(), 1);
            assert_eq!(manifest.holdings.len(), 1);
            assert_eq!(manifest.holdings[0].document, "doc-a");
            assert_eq!(manifest.holdings[0].revision, 1);
            assert_eq!(manifest.holdings[0].digest, digest(&auth_before));
            // The outgoing manifest round-trips as a client message.
            let _ = ClientMessage::Manifest(manifest);
        }
        other => panic!("expected NeedManifest, got {other:?}"),
    }
    // Nothing diverged silently.
    assert_eq!(client.authoritative()["doc-a"], auth_before);
    assert_eq!(client.view()["doc-a"], view_before);
    assert_eq!(client.pending().len(), 1);
}

#[test]
fn incompatible_replication_requests_manifest() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let auth_before = client.authoritative()["doc-a"].clone();

    let base = snapshot("doc-a", 1, 0);
    let mut intent = Intent {
        id: 5,
        document: "doc-a".into(),
        version: "2".into(),
        mutation: "inc".into(),
        args: json!({}),
    };
    // Compute a plausible result digest anyway; version mismatch must still fail.
    let applied = snapshot("doc-a", 2, 1);
    let replication = Replication {
        base_revision: base.revision,
        base_digest: digest(&base),
        revision: applied.revision,
        result_digest: digest(&applied),
        intent: intent.clone(),
        actor: "bob".into(),
    };
    intent.version = "2".into();
    let _ = intent;
    let outcome = client
        .handle(&registry, ServerMessage::Replication(replication))
        .unwrap();
    assert!(matches!(
        outcome,
        Outcome::NeedManifest {
            error: Error::Incompatible,
            ..
        }
    ));
    assert_eq!(client.authoritative()["doc-a"], auth_before);
}

#[test]
fn unknown_document_replication_requests_manifest_without_creating() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);

    let ghost_base = snapshot("ghost", 1, 0);
    let intent = Intent {
        id: 3,
        document: "ghost".into(),
        version: "1".into(),
        mutation: "inc".into(),
        args: json!({}),
    };
    let applied = registry.apply(&ghost_base, &intent, "bob").unwrap();
    let replication = Replication {
        base_revision: ghost_base.revision,
        base_digest: digest(&ghost_base),
        revision: applied.revision,
        result_digest: digest(&applied),
        intent,
        actor: "bob".into(),
    };
    let outcome = client
        .handle(&registry, ServerMessage::Replication(replication))
        .unwrap();
    match outcome {
        Outcome::NeedManifest { manifest, error } => {
            assert_eq!(error, Error::NotFound);
            assert!(
                !manifest
                    .holdings
                    .iter()
                    .any(|held| held.document == "ghost")
            );
        }
        other => panic!("expected NeedManifest, got {other:?}"),
    }
    assert!(!client.authoritative().contains_key("ghost"));
    assert!(!client.view().contains_key("ghost"));
}

#[test]
fn recovered_receipt_does_not_roll_back_newer_snapshot() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);

    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();

    // Replacement is much newer than the carried receipt.
    let replacement = snapshot("doc-a", 5, 100);
    let stale_receipt_snapshot = snapshot("doc-a", 2, 1);
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![replacement.clone()],
                completed: vec![Completion {
                    id: id1,
                    document: "doc-a".into(),
                    result: Ok(Some(stale_receipt_snapshot)),
                }],
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    // Entry removed, but the replacement wins; no rollback to rev2.
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, id2);
    assert_eq!(client.authoritative()["doc-a"], replacement);
    assert_eq!(client.authoritative()["doc-a"].revision, 5);
    // Retained intent replays over the replacement: 100 + 1 -> rev6 count101.
    assert_eq!(client.view()["doc-a"].revision, 6);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 101 }));
}

#[test]
fn stale_direct_completion_does_not_roll_back() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();

    // Advance authoritative past the completion via remote replication first.
    let remote = Intent {
        id: 50,
        document: "doc-a".into(),
        version: "1".into(),
        mutation: "set".into(),
        args: json!({ "value": 40 }),
    };
    let base = snapshot("doc-a", 1, 0);
    let replication = replication_for(&registry, &base, &remote, "bob");
    // Remote set 0 -> 40 gives rev2; local inc replays to rev3 count41.
    client
        .handle(&registry, ServerMessage::Replication(replication))
        .unwrap();
    assert_eq!(client.authoritative()["doc-a"].revision, 2);

    // Now the stale completion for the local intent arrives with rev2 count1,
    // older than current authoritative rev2 count40 (equal revision, older
    // value) or newer logic: it must not replace the newer state.
    let stale = snapshot("doc-a", 2, 1);
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: id1,
                document: "doc-a".into(),
                result: Ok(Some(stale)),
            }),
        )
        .unwrap();
    assert_eq!(outcome, Outcome::Completed { id: id1 });
    assert_eq!(
        client.authoritative()["doc-a"].value,
        json!({ "count": 40 })
    );
    assert!(client.pending().is_empty());
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 40 }));
}

#[test]
fn expiry_clears_and_does_not_replay() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let _ = mutate_message(&mut client);
    assert!(!client.pending().is_empty());

    let outcome = client.handle(&registry, ServerMessage::Reset).unwrap();
    assert_eq!(outcome, Outcome::Reset);
    assert!(client.authoritative().is_empty());
    assert!(client.view().is_empty());
    assert!(client.pending().is_empty());
    assert!(client.next_submission().is_none());
    assert!(client.is_reconciling());
    // Expired writes are never replayed: the fresh manifest is empty.
    let manifest = client.manifest();
    assert!(manifest.holdings.is_empty());
    assert!(manifest.pending.is_empty());

    // No mutations until the fresh reconciliation arrives.
    assert_eq!(
        client.enqueue(&registry, "doc-a", "inc", json!({})),
        Err(Error::Protocol)
    );

    // Fresh lifetime rebuilds from scratch; old intents never reappear.
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 99)]);
    assert!(!client.is_reconciling());
    assert!(client.pending().is_empty());
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 99 }));
    let id = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 100 }));
    assert_eq!(client.pending()[0].id, id);
}

#[test]
fn rejection_preserves_subsequent_edits() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "add", json!({ "delta": 10 }))
        .unwrap();
    let _ = mutate_message(&mut client);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();

    let outcome = client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: id1,
                document: "doc-a".into(),
                result: Err(Error::Rejected("denied by guard".into())),
            }),
        )
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::Rejected {
            id: id1,
            error: Error::Rejected("denied by guard".into())
        }
    );
    // Only the rejected entry leaves; the later edit is preserved and rebased
    // onto the unchanged base (rev1 count0 +10 -> rev2 count10).
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, id2);
    assert_eq!(client.authoritative()["doc-a"].value, json!({ "count": 0 }));
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 10 }));
    assert_eq!(client.view()["doc-a"].revision, 2);
}

#[test]
fn forbidden_drops_document_and_pending() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(
        &mut client,
        &registry,
        vec![snapshot("doc-a", 1, 0), snapshot("doc-b", 1, 5)],
    );
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let other = client
        .enqueue(&registry, "doc-b", "inc", json!({}))
        .unwrap();

    let outcome = client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: id1,
                document: "doc-a".into(),
                result: Ok(None),
            }),
        )
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::Forbidden {
            id: id1,
            document: "doc-a".into()
        }
    );
    assert!(!client.authoritative().contains_key("doc-a"));
    assert!(!client.view().contains_key("doc-a"));
    assert!(
        client
            .pending()
            .iter()
            .all(|intent| intent.document != "doc-a")
    );
    // The unrelated document and its pending survive.
    assert!(client.authoritative().contains_key("doc-b"));
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, other);
}

#[test]
fn revocation_clears_and_blocks_reintroduction() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(
        &mut client,
        &registry,
        vec![snapshot("doc-a", 1, 0), snapshot("doc-b", 1, 7)],
    );
    let pending_a = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let _ = pending_a;
    let pending_b = client
        .enqueue(&registry, "doc-b", "inc", json!({}))
        .unwrap();

    let outcome = client
        .handle(&registry, ServerMessage::Removed(vec!["doc-a".into()]))
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::Removed {
            documents: vec!["doc-a".into()]
        }
    );
    assert!(!client.authoritative().contains_key("doc-a"));
    assert!(!client.view().contains_key("doc-a"));
    assert!(
        client
            .pending()
            .iter()
            .all(|intent| intent.document != "doc-a")
    );
    // Unrelated holdings and pending survive.
    assert!(client.authoritative().contains_key("doc-b"));
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, pending_b);

    // New local edits for the revoked document are observably rejected.
    assert_eq!(
        client.enqueue(&registry, "doc-a", "inc", json!({})),
        Err(Error::Denied)
    );
    // Queued remote delivery must not reintroduce it either.
    let ghost_base = snapshot("doc-a", 1, 0);
    let intent = Intent {
        id: 11,
        document: "doc-a".into(),
        version: "1".into(),
        mutation: "inc".into(),
        args: json!({}),
    };
    let applied = registry.apply(&ghost_base, &intent, "bob").unwrap();
    let replication = Replication {
        base_revision: ghost_base.revision,
        base_digest: digest(&ghost_base),
        revision: applied.revision,
        result_digest: digest(&applied),
        intent,
        actor: "bob".into(),
    };
    assert_eq!(
        client.handle(&registry, ServerMessage::Replication(replication)),
        Err(Error::Denied)
    );
    assert!(!client.view().contains_key("doc-a"));
}

#[test]
fn surviving_reconnect_resubmits_same_ids_after_reconciliation() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let first = mutate_message(&mut client);
    assert_eq!(first.id, id1);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();

    client.begin_reconnect();
    assert!(client.is_reconciling());
    // Retained journal is presented; nothing may leave until reconciled.
    let manifest: Manifest = client.manifest();
    assert_eq!(manifest.pending.len(), 2);
    assert_eq!(manifest.pending[0].id, id1);
    assert_eq!(manifest.pending[1].id, id2);
    assert!(client.next_submission().is_none());
    assert_eq!(
        client.enqueue(&registry, "doc-a", "inc", json!({})),
        Err(Error::Protocol)
    );

    // Server committed nothing while we were gone: both resubmit with the
    // same stable IDs, oldest first, paced by fresh acceptances.
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![snapshot("doc-a", 1, 0)],
                completed: Vec::new(),
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    assert!(!client.is_reconciling());
    let resend1 = mutate_message(&mut client);
    assert_eq!(resend1.id, id1);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();
    let resend2 = mutate_message(&mut client);
    assert_eq!(resend2.id, id2);
}

#[test]
fn surviving_reconnect_dedups_committed_receipts() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    client.begin_reconnect();

    // The server did commit the first intent while the stream was down; its
    // receipt arrives inside the reconciliation.
    let committed = snapshot("doc-a", 2, 1);
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![committed.clone()],
                completed: vec![Completion {
                    id: id1,
                    document: "doc-a".into(),
                    result: Ok(Some(committed.clone())),
                }],
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    // Only the unknown intent resubmits, with its original ID.
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, id2);
    let resend = mutate_message(&mut client);
    assert_eq!(resend.id, id2);
    assert_eq!(client.authoritative()["doc-a"], committed);
}

#[test]
fn stale_completion_is_ignored_without_rollback() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let revision_before = client.revision();
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: 999,
                document: "doc-a".into(),
                result: Ok(Some(snapshot("doc-a", 2, 1))),
            }),
        )
        .unwrap();
    assert_eq!(outcome, Outcome::Ignored { id: 999 });
    assert_eq!(client.authoritative()["doc-a"], snapshot("doc-a", 1, 0));
    assert_eq!(client.revision(), revision_before);
}

#[test]
fn replay_error_surfaces_without_dropping_journal() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    client
        .enqueue(&registry, "doc-a", "inc_capped", json!({}))
        .unwrap();
    client
        .enqueue(&registry, "doc-a", "inc_capped", json!({}))
        .unwrap();
    assert_eq!(client.pending().len(), 2);

    // Remote jumps the base to count10; the capped replay (10+1 > 3) fails.
    let remote = Intent {
        id: 60,
        document: "doc-a".into(),
        version: "1".into(),
        mutation: "set".into(),
        args: json!({ "value": 10 }),
    };
    let base = snapshot("doc-a", 1, 0);
    let replication = replication_for(&registry, &base, &remote, "bob");
    let result = client.handle(&registry, ServerMessage::Replication(replication));
    assert_eq!(result, Err(Error::Invalid));
    // The journal is retained, nothing was silently dropped.
    assert_eq!(client.pending().len(), 2);
    // Authoritative did advance; the view holds the new base (prefix before
    // the failing replay) rather than a divergent value.
    assert_eq!(
        client.authoritative()["doc-a"].value,
        json!({ "count": 10 })
    );
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 10 }));
}

#[test]
fn enqueue_validation_is_explicit() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    assert_eq!(
        client.enqueue(&registry, "missing", "inc", json!({})),
        Err(Error::NotFound)
    );
    assert_eq!(
        client.enqueue(&registry, "", "inc", json!({})),
        Err(Error::Invalid)
    );
    assert_eq!(
        client.enqueue(&registry, "doc-a", "", json!({})),
        Err(Error::Invalid)
    );
    // Bad args fail replay but the intent is still journaled; the error is
    // surfaced instead of silently dropping the entry.
    let pending_before = client.pending().len();
    let result = client.enqueue(&registry, "doc-a", "add", json!({}));
    assert_eq!(result, Err(Error::Invalid));
    assert_eq!(client.pending().len(), pending_before + 1);
}

#[test]
fn manifest_holdings_match_authoritative() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(
        &mut client,
        &registry,
        vec![snapshot("doc-b", 2, 5), snapshot("doc-a", 1, 0)],
    );
    let manifest = client.manifest();
    // Sorted document order from the authoritative map.
    assert_eq!(manifest.holdings.len(), 2);
    assert_eq!(manifest.holdings[0].document, "doc-a");
    assert_eq!(manifest.holdings[1].document, "doc-b");
    assert_eq!(
        manifest.holdings[0].digest,
        digest(&snapshot("doc-a", 1, 0))
    );
    assert_eq!(
        manifest.holdings[1].digest,
        digest(&snapshot("doc-b", 2, 5))
    );
    assert!(manifest.pending.is_empty());

    client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let manifest = client.manifest();
    assert_eq!(manifest.pending.len(), 1);
    // Holdings still describe the authoritative base, not the optimistic view.
    assert_eq!(manifest.holdings[0].revision, 1);
}

#[test]
fn revision_advances_only_on_publication() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let base = client.revision();
    client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    assert_eq!(client.revision(), base + 1);
    let after_enqueue = client.revision();
    let intent = mutate_message(&mut client);
    client
        .handle(&registry, ServerMessage::Accepted { id: intent.id })
        .unwrap();
    // Acceptance never notifies.
    assert_eq!(client.revision(), after_enqueue);
    // Stale receipts never notify either.
    client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: 4242,
                document: "doc-a".into(),
                result: Ok(Some(snapshot("doc-a", 9, 9))),
            }),
        )
        .unwrap();
    assert_eq!(client.revision(), after_enqueue);
}

#[test]
fn deterministic_schedules_property_loop() {
    // Varied interleavings over two documents and three mutations. Every
    // schedule is deterministic in the seed; each step asserts the portable
    // invariants (nonzero increasing IDs, at most one awaiting ACK, revision
    // coherence, manifest agreement).
    for seed in 0..64_u64 {
        let registry = registry();
        let mut client = Client::new("alice".into());
        install(
            &mut client,
            &registry,
            vec![snapshot("doc-a", 1, 0), snapshot("doc-b", 1, 100)],
        );
        let mut last_revision = client.revision();
        let mut seen_ids: Vec<u64> = Vec::new();

        for step in 0..12_u64 {
            let selector = (seed.wrapping_mul(31).wrapping_add(step.wrapping_mul(17))) % 6;
            match selector {
                0 => {
                    let document = if step % 2 == 0 { "doc-a" } else { "doc-b" };
                    if let Ok(id) = client.enqueue(&registry, document, "inc", json!({})) {
                        assert_ne!(id, 0);
                        assert!(!seen_ids.contains(&id));
                        seen_ids.push(id);
                        assert!(client.revision() >= last_revision);
                        last_revision = client.revision();
                    }
                }
                1 => {
                    if let Some(message) = client.next_submission() {
                        let ClientMessage::Mutate(intent) = message else {
                            panic!("expected Mutate");
                        };
                        assert!(client.pending().iter().any(|queued| queued.id == intent.id));
                        let outcome = client
                            .handle(&registry, ServerMessage::Accepted { id: intent.id })
                            .unwrap();
                        assert_eq!(outcome, Outcome::Accepted { id: intent.id });
                    }
                }
                2 => {
                    // Complete the oldest accepted-but-uncompleted intent with
                    // its true authoritative effect when possible.
                    let candidate = client
                        .pending()
                        .iter()
                        .find(|intent| {
                            client
                                .awaiting_ack()
                                .is_none_or(|awaiting| awaiting != intent.id)
                        })
                        .cloned();
                    if let Some(intent) = candidate
                        && let Some(base) = client.authoritative().get(&intent.document).cloned()
                        && let Ok(committed) = registry.apply(&base, &intent, "alice")
                        && base.revision == client.authoritative()[&intent.document].revision
                    {
                        // Only complete intents whose base still matches;
                        // otherwise the server would have replicated first.
                        let outcome = client
                            .handle(
                                &registry,
                                ServerMessage::Completed(Completion {
                                    id: intent.id,
                                    document: intent.document.clone(),
                                    result: Ok(Some(committed)),
                                }),
                            )
                            .unwrap();
                        assert!(matches!(
                            outcome,
                            Outcome::Completed { .. } | Outcome::Ignored { .. }
                        ));
                        last_revision = client.revision();
                    }
                }
                3 => {
                    // Remote set on doc-b from bob, verified against the
                    // current authoritative base.
                    if let Some(base) = client.authoritative().get("doc-b").cloned() {
                        let intent = Intent {
                            id: 1000 + seed * 100 + step,
                            document: "doc-b".into(),
                            version: "1".into(),
                            mutation: "inc".into(),
                            args: json!({}),
                        };
                        // Skip when the optimistic replay would exceed the
                        // capped range used elsewhere; this schedule only uses
                        // uncapped inc/add here, so apply succeeds.
                        let replication = replication_for(&registry, &base, &intent, "bob");
                        let result =
                            client.handle(&registry, ServerMessage::Replication(replication));
                        // Uncapped inc always verifies; the view must rebase.
                        assert!(result.is_ok());
                        last_revision = client.revision();
                    }
                }
                4 => {
                    // Divergent delivery must request recovery, never apply.
                    let bad = Replication {
                        intent: Intent {
                            id: 1,
                            document: "doc-a".into(),
                            version: "1".into(),
                            mutation: "inc".into(),
                            args: json!({}),
                        },
                        actor: "mallory".into(),
                        base_revision: 9999,
                        base_digest: "bad".into(),
                        revision: 9999,
                        result_digest: "bad".into(),
                    };
                    let auth_before = client.authoritative().clone();
                    let outcome = client
                        .handle(&registry, ServerMessage::Replication(bad))
                        .unwrap();
                    assert!(matches!(outcome, Outcome::NeedManifest { .. }));
                    assert_eq!(client.authoritative(), &auth_before);
                }
                _ => {
                    let manifest = client.manifest();
                    assert_eq!(manifest.pending.len(), client.pending().len());
                    for (held, pending) in manifest.pending.iter().zip(client.pending().iter()) {
                        assert_eq!(held, pending);
                    }
                    for holding in &manifest.holdings {
                        let held = &client.authoritative()[&holding.document];
                        assert_eq!(holding.revision, held.revision);
                        assert_eq!(holding.digest, digest(held));
                    }
                }
            }

            // Portable invariants after every step.
            let mut sorted = seen_ids.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), seen_ids.len(), "intent IDs must be unique");
            assert!(seen_ids.iter().all(|id| *id != 0), "IDs are nonzero");
            for window in seen_ids.windows(2) {
                assert!(window[0] < window[1], "IDs increase monotonically");
            }
            assert!(client.revision() >= last_revision);
            last_revision = client.revision();
            // View coherence: every projected revision is at least its base,
            // and every pending intent targets a held (or forward-held) doc.
            for (document, projected) in client.view() {
                if let Some(base) = client.authoritative().get(document) {
                    assert!(projected.revision >= base.revision);
                    assert_eq!(projected.id, *document);
                }
            }
            if let Some(awaiting) = client.awaiting_ack() {
                assert!(client.pending().iter().any(|intent| intent.id == awaiting));
            }
        }
    }
}

#[test]
fn unsolicited_holdings_refresh_preserves_accepted_pacing() {
    // Steady-state Access gain: whole replacement arrives without an explicit
    // manifest request. Already-accepted intents must not requeue.
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let first = mutate_message(&mut client);
    assert_eq!(first.id, id1);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();
    assert!(!client.needs_recovery());

    // Unsolicited replacement adds doc-b while keeping doc-a at rev1.
    // No receipts: nothing was explicitly requested.
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![snapshot("doc-a", 1, 0), snapshot("doc-b", 1, 7)],
                completed: Vec::new(),
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    assert!(!client.is_reconciling());
    assert!(!client.needs_recovery());
    // Journal untouched; acceptance marks preserved.
    assert_eq!(client.pending().len(), 2);
    assert!(client.authoritative().contains_key("doc-b"));
    // Next submission is still the second intent, not a requeue of the first.
    let next = mutate_message(&mut client);
    assert_eq!(next.id, id2);
    // Duplicate acceptance for the first stays idempotent, not a protocol error.
    assert_eq!(
        client
            .handle(&registry, ServerMessage::Accepted { id: id1 })
            .unwrap(),
        Outcome::Accepted { id: id1 }
    );
}

#[test]
fn recovery_manifest_requeues_unresolved_after_need_manifest() {
    // Divergence arms recovery; only the correlated reconciliation requeues.
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let first = mutate_message(&mut client);
    assert_eq!(first.id, id1);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();

    let bad = Replication {
        intent: Intent {
            id: 99,
            document: "doc-a".into(),
            version: "1".into(),
            mutation: "inc".into(),
            args: json!({}),
        },
        actor: "mallory".into(),
        base_revision: 999,
        base_digest: "bad".into(),
        revision: 999,
        result_digest: "bad".into(),
    };
    let outcome = client
        .handle(&registry, ServerMessage::Replication(bad))
        .unwrap();
    assert!(matches!(outcome, Outcome::NeedManifest { .. }));
    assert!(client.needs_recovery());

    // Correlated recovery answer carries the same base; nothing committed.
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![snapshot("doc-a", 1, 0)],
                completed: Vec::new(),
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    assert!(!client.needs_recovery());
    // Both intents resubmit with stable IDs, oldest first.
    let resend1 = mutate_message(&mut client);
    assert_eq!(resend1.id, id1);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();
    let resend2 = mutate_message(&mut client);
    assert_eq!(resend2.id, id2);
}

#[test]
fn origin_completion_before_same_commit_holdings_refresh() {
    // Host queues the originator's completion before the same-commit
    // holdings refresh. Handling in arrival order applies once and the
    // refresh preserves pacing for the still-pending second intent.
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(&mut client, &registry, vec![snapshot("doc-a", 1, 0)]);
    let id1 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let id2 = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    let first = mutate_message(&mut client);
    assert_eq!(first.id, id1);
    client
        .handle(&registry, ServerMessage::Accepted { id: id1 })
        .unwrap();
    let second = mutate_message(&mut client);
    assert_eq!(second.id, id2);
    client
        .handle(&registry, ServerMessage::Accepted { id: id2 })
        .unwrap();

    let committed = snapshot("doc-a", 2, 1);
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Completed(Completion {
                id: id1,
                document: "doc-a".into(),
                result: Ok(Some(committed.clone())),
            }),
        )
        .unwrap();
    assert_eq!(outcome, Outcome::Completed { id: id1 });

    // Same-commit refresh carries the just-committed revision for everyone.
    let outcome = client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![committed.clone()],
                completed: Vec::new(),
            }),
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    // No double apply: authoritative stays rev2, retained intent rebases once.
    assert_eq!(client.authoritative()["doc-a"], committed);
    assert_eq!(client.pending().len(), 1);
    assert_eq!(client.pending()[0].id, id2);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 2 }));
    assert_eq!(client.view()["doc-a"].revision, 3);
}

#[test]
fn revocation_tombstone_lifts_only_on_regrant() {
    let registry = registry();
    let mut client = Client::new("alice".into());
    install(
        &mut client,
        &registry,
        vec![snapshot("doc-a", 1, 0), snapshot("doc-b", 1, 0)],
    );
    client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    client
        .handle(&registry, ServerMessage::Removed(vec!["doc-a".into()]))
        .unwrap();
    assert_eq!(
        client.enqueue(&registry, "doc-a", "inc", json!({})),
        Err(Error::Denied)
    );

    // Unsolicited refresh that still omits doc-a keeps the tombstone.
    client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![snapshot("doc-b", 1, 0)],
                completed: Vec::new(),
            }),
        )
        .unwrap();
    assert_eq!(
        client.enqueue(&registry, "doc-a", "inc", json!({})),
        Err(Error::Denied)
    );

    // Re-grant arrives as a replacement containing doc-a again: the tombstone
    // lifts and local edits are accepted with fresh IDs.
    client
        .handle(
            &registry,
            ServerMessage::Manifest(Reconciliation {
                documents: vec![snapshot("doc-a", 1, 0), snapshot("doc-b", 1, 0)],
                completed: Vec::new(),
            }),
        )
        .unwrap();
    let id = client
        .enqueue(&registry, "doc-a", "inc", json!({}))
        .unwrap();
    assert_ne!(id, 0);
    assert_eq!(client.view()["doc-a"].value, json!({ "count": 1 }));
}
