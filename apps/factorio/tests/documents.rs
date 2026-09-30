use factorio::{Command, Config, Status, Ticket, workspaces as graph};
use snap_access::{Actor, ChangeSet, GrantChange, Resource, Role};
use snap_document::Intent;
use snap_store::{Error, Store};

const ROOT: &str = "a0000000-0000-4000-8000-000000000001";
const OTHER: &str = "a0000000-0000-4000-8000-000000000002";

/// Exercise actual portable application admission and commit rather than asking
/// persistence helpers to authorize a caller. Store fixtures remain real SQLite.
fn dispatch(
    store: &mut snap_store::Store<snap_sqlite::Sqlite>,
    actor: &str,
    name: &str,
    input: serde_json::Value,
) -> snap_transport::Outcome {
    use snap_transport::operation::{Context, Runtime};
    let mut runtime = Runtime::default();
    for definition in factorio::application(config(), "http://candidate.invalid".into()).requests {
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
        inputs: [("clock".into(), serde_json::json!(0))]
            .into_iter()
            .collect(),
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
fn store() -> Store<snap_sqlite::Sqlite> {
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    store
}
fn config() -> Config {
    Config {
        repository: "/existing/repo".into(),
        mainline: "main".into(),
        modules: [("one".into(), "apps/one".into())].into_iter().collect(),
        resources: "/resources".into(),
        first_port: 10000,
        setup: vec![],
        teardown: vec![],
    }
}
fn ticket(id: &str) -> Ticket {
    Ticket {
        id: id.into(),
        created_at: None,
        title: "Implement one".into(),
        description: "A bounded change".into(),
        modules: vec!["one".into()],
        status: Status::Ready,
        notes: String::new(),
        parent: None,
        blockers: vec![],
    }
}
fn start(id: &str) -> Command {
    Command::Start {
        id: id.into(),
        prompt: "Work on one".into(),
        tickets: vec!["one".into()],
        modules: vec![],
        base: "a".repeat(40),
        conversation: format!("ses_{id}"),
    }
}

#[test]
fn creation_dates_are_server_owned_and_survive_edits_and_legacy_snapshots() {
    let mut store = store();
    let mut supplied = ticket("one");
    supplied.created_at = Some(999);
    store
        .run("create dated work", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                100,
                Command::Ticket { ticket: supplied },
            )?;
            graph::command(tx, ROOT, "alice", false, 200, start("first"))
        })
        .unwrap();
    let mut value = store
        .inspect("creation dates", |tx| {
            Ok(serde_json::to_value(graph::load(tx, ROOT, "alice")?).unwrap())
        })
        .unwrap();
    assert_eq!(value["tickets"]["one"]["created_at"], 100);
    assert_eq!(value["sessions"]["first"]["created_at"], 200);
    value["tickets"]["one"]["created_at"] = 999.into();
    let mut edited: Ticket = serde_json::from_value(value["tickets"]["one"].clone()).unwrap();
    edited.title = "Updated title".into();
    store
        .run("edit without moving creation date", |tx| {
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                300,
                Command::Ticket { ticket: edited },
            )
        })
        .unwrap();
    store
        .inspect("date retained", |tx| {
            let current = serde_json::to_value(graph::load(tx, ROOT, "alice")?).unwrap();
            assert_eq!(current["tickets"]["one"]["created_at"], 100);
            Ok(())
        })
        .unwrap();
    store
        .run("load a legacy undated ticket", |tx| {
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("legacy"),
                },
            )?;
            let id = graph::child_id(ROOT, graph::TICKET_KIND, "legacy");
            let mut snapshot = graph::document().read(tx, &id, Some("alice"))?.value;
            snapshot["data"]
                .as_object_mut()
                .unwrap()
                .remove("created_at");
            graph::document().replace(tx, &id, "alice", snapshot)?;
            Ok(())
        })
        .unwrap();
    for (record, expected) in [("one", Some(100)), ("legacy", None)] {
        for (n, supplied) in [
            Some(serde_json::json!(999)),
            Some(serde_json::Value::Null),
            None,
        ]
        .into_iter()
        .enumerate()
        {
            let mut args = store
                .inspect("read editable ticket", |tx| {
                    Ok(
                        serde_json::to_value(&graph::load(tx, ROOT, "alice")?.tickets[record])
                            .unwrap(),
                    )
                })
                .unwrap();
            let title = format!("Named edit {record} {n}");
            args["title"] = title.clone().into();
            if let Some(date) = supplied {
                args["created_at"] = date;
            } else {
                args.as_object_mut().unwrap().remove("created_at");
            }
            let intent = Intent {
                id: (n + if record == "one" { 1 } else { 4 }) as u64,
                document: graph::child_id(ROOT, graph::TICKET_KIND, record),
                mutation: "ticket.edit".into(),
                version: "1".into(),
                args,
            };
            let accepted = store
                .inspect("admit named edit", |tx| {
                    graph::document().access_guard().admit(tx, "alice", &intent)
                })
                .unwrap()
                .unwrap();
            store
                .run("commit named edit", |tx| {
                    let result =
                        graph::document().execute_recorded(tx, "creation-date-test", accepted)?;
                    assert!(result.completion.result.is_ok());
                    Ok(())
                })
                .unwrap();
            store
                .inspect("named edit preserves creation date", |tx| {
                    let state = graph::load(tx, ROOT, "alice")?;
                    assert_eq!(state.tickets[record].title, title);
                    assert_eq!(state.tickets[record].created_at, expected);
                    Ok(())
                })
                .unwrap();
        }
    }
    value["tickets"]["one"]
        .as_object_mut()
        .unwrap()
        .remove("created_at");
    value["sessions"]["first"]
        .as_object_mut()
        .unwrap()
        .remove("created_at");
    let legacy: factorio::Workspace = serde_json::from_value(value).unwrap();
    let legacy = serde_json::to_value(legacy).unwrap();
    assert!(legacy["tickets"]["one"]["created_at"].is_null());
    assert!(legacy["sessions"]["first"]["created_at"].is_null());
}

#[test]
fn linked_documents_inherit_workspace_access_without_a_service_owner() {
    let mut store = store();
    store
        .run("onboard", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::onboard(tx, OTHER, "bob", config())?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("one"),
                },
            )?;
            graph::command(
                tx,
                OTHER,
                "bob",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("one"),
                },
            )?;
            Ok(())
        })
        .unwrap();
    let id = graph::child_id(ROOT, graph::TICKET_KIND, "one");
    assert_ne!(id, graph::child_id(OTHER, graph::TICKET_KIND, "one"));
    store
        .inspect("isolated graphs", |tx| {
            assert_eq!(
                graph::document().access_guard().extent(tx, "alice")?.len(),
                2
            );
            Ok(())
        })
        .unwrap();
    assert!(
        dispatch(
            &mut store,
            "bob",
            "factorio.workspace",
            serde_json::json!({"workspace":ROOT})
        )
        .is_err()
    );
    assert!(
        dispatch(
            &mut store,
            "alice",
            "factorio.workspace",
            serde_json::json!({"workspace":ROOT})
        )
        .is_ok()
    );
    store
        .run("transfer workspace", |tx| {
            let mut change = ChangeSet::new(Actor::identity("alice")?);
            for (identity, role) in [("bob", Some(Role::Owner)), ("alice", None)] {
                change.grants.push(GrantChange {
                    resource: Resource::new("document", ROOT)?,
                    identity: identity.into(),
                    role,
                });
            }
            snap_document::access::vocabulary().change(tx, &change)?;
            Ok(())
        })
        .unwrap();
    store
        .inspect("inherited transfer", |tx| {
            assert!(
                graph::document()
                    .access_guard()
                    .extent(tx, "alice")?
                    .is_empty()
            );
            assert_eq!(graph::load(tx, ROOT, "bob")?.tickets.len(), 1);
            Ok(())
        })
        .unwrap();
    assert!(
        dispatch(
            &mut store,
            "alice",
            "factorio.workspace",
            serde_json::json!({"workspace":ROOT})
        )
        .is_err()
    );
    assert!(
        dispatch(
            &mut store,
            "bob",
            "factorio.workspace",
            serde_json::json!({"workspace":ROOT})
        )
        .is_ok()
    );
}

#[test]
fn replacements_get_fresh_document_and_conversation_incarnations() {
    let mut store = store();
    let first = store
        .run("initial", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("one"),
                },
            )?;
            graph::create_intake(tx, ROOT, "alice", "request", "Original")
        })
        .unwrap()
        .value;
    let old_ticket = graph::child_id(ROOT, graph::TICKET_KIND, "one");
    let old_intake = graph::child_id(ROOT, graph::INTAKE_KIND, "request");
    store
        .run("delete", |tx| {
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::DeleteTicket { id: "one".into() },
            )?;
            graph::delete_intake(tx, ROOT, "alice", "request")
        })
        .unwrap();
    assert!(
        dispatch(
            &mut store,
            "bob",
            "factorio.intake-create",
            serde_json::json!({"workspace":ROOT,"id":"request","description":"Replacement"})
        )
        .is_err()
    );
    let replaced = store
        .run("recreate", |tx| {
            let mut replacement = ticket("one");
            replacement.title = "Replacement".into();
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: replacement,
                },
            )?;
            graph::create_intake(tx, ROOT, "alice", "request", "Replacement")
        })
        .unwrap()
        .value;
    assert_ne!(first.conversation, replaced.conversation);
    store
        .inspect("new identities", |tx| {
            let root = graph::root(tx, ROOT, "alice")?;
            assert_ne!(root.tickets["one"], old_ticket);
            assert_ne!(root.intakes["request"], old_intake);
            assert_eq!(
                graph::load(tx, ROOT, "alice")?.tickets["one"].title,
                "Replacement"
            );
            assert_eq!(
                graph::document().retained(tx, &old_ticket)?.value["data"]["title"],
                "Implement one"
            );
            let stale = Intent {
                id: 1,
                document: old_ticket.clone(),
                mutation: "ticket.edit".into(),
                version: "1".into(),
                args: serde_json::to_value(ticket("one")).unwrap(),
            };
            assert!(
                graph::document()
                    .access_guard()
                    .admit(tx, "alice", &stale)?
                    .is_err()
            );
            let active = graph::document().access_guard().extent(tx, "alice")?;
            assert_eq!(active.len(), 3);
            assert!(!active.contains(&old_ticket));
            assert!(!active.contains(&old_intake));
            Ok(())
        })
        .unwrap();
    // Controller completion targets the current incarnation, not the tombstone.
    store
        .run("complete replacement ticket", |tx| {
            graph::command(tx, ROOT, "alice", false, 0, start("work"))?;
            graph::observe(tx, ROOT, "work", factorio::Effect::Started)?;
            graph::observe(
                tx,
                ROOT,
                "work",
                factorio::Effect::Published {
                    commit: "b".repeat(40),
                    target: "a".repeat(40),
                    evidence: "checked".into(),
                    findings: vec![],
                },
            )?;
            graph::command(
                tx,
                ROOT,
                "alice",
                true,
                0,
                Command::Approve {
                    id: "work".into(),
                    commit: "b".repeat(40),
                },
            )?;
            graph::observe(
                tx,
                ROOT,
                "work",
                factorio::Effect::Integrating {
                    commit: "c".repeat(40),
                },
            )?;
            graph::observe(tx, ROOT, "work", factorio::Effect::Integrated)
        })
        .unwrap();
    store
        .inspect("current ticket completed", |tx| {
            assert_eq!(
                graph::load(tx, ROOT, "alice")?.tickets["one"].status,
                Status::Done
            );
            assert_eq!(
                graph::document().retained(tx, &old_ticket)?.value["data"]["status"],
                "ready"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn intake_drafts_are_independent_documents_with_atomic_revision_guards() {
    let mut store = store();
    store
        .run("intake", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::create_intake(tx, ROOT, "alice", "request", "Build the app")
        })
        .unwrap();
    let mut first = ticket("request-first");
    first.status = Status::Draft;
    let mut second = ticket("request-second");
    second.status = Status::Draft;
    second.blockers = vec![first.id.clone()];
    let batch = factorio::intake::Drafts {
        revision: 0,
        route: factorio::intake::Route::Implement,
        rationale: "Two ordered changes".into(),
        tickets: vec![first.clone(), second],
    };
    store
        .run("draft batch", |tx| {
            graph::drafts(tx, ROOT, "alice", "request", batch.clone(), 400)
        })
        .unwrap();
    assert!(
        store
            .run("stale batch", |tx| graph::drafts(
                tx, ROOT, "alice", "request", batch, 500
            ))
            .is_err()
    );
    let snapshot = store
        .inspect("documents", |tx| {
            assert_eq!(
                graph::document().access_guard().extent(tx, "alice")?.len(),
                4
            );
            assert_eq!(
                graph::load(tx, ROOT, "alice")?.tickets["request-first"].created_at,
                Some(400)
            );
            assert_eq!(
                graph::load(tx, ROOT, "alice")?.intakes["request"].revision,
                1
            );
            graph::document().read(
                tx,
                &graph::child_id(ROOT, graph::INTAKE_KIND, "request"),
                Some("alice"),
            )
        })
        .unwrap();
    let mut invalid = ticket("request-invalid");
    invalid.status = Status::Draft;
    invalid.blockers = vec!["absent".into()];
    first.title = "Must roll back".into();
    let rejected = factorio::intake::Drafts {
        revision: 1,
        route: factorio::intake::Route::Implement,
        rationale: "Invalid dependency".into(),
        tickets: vec![first, invalid],
    };
    assert!(
        store
            .run("invalid batch", |tx| graph::drafts(
                tx, ROOT, "alice", "request", rejected, 600
            ))
            .is_err()
    );
    store
        .inspect("unchanged", |tx| {
            assert_eq!(
                graph::document().read(tx, &snapshot.id, Some("alice"))?,
                snapshot
            );
            assert_eq!(
                graph::load(tx, ROOT, "alice")?.tickets["request-first"].title,
                "Implement one"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn deleted_session_retains_claims_and_cleanup_finalizer() {
    let mut store = store();
    let id = graph::child_id(ROOT, graph::SESSION_KIND, "first");
    store
        .run("start and delete", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("one"),
                },
            )?;
            graph::command(tx, ROOT, "alice", false, 0, start("first"))?;
            graph::document().remove(tx, &id, "alice")
        })
        .unwrap();
    store
        .inspect("cleanup still owns claims", |tx| {
            assert!(
                !graph::document()
                    .access_guard()
                    .extent(tx, "alice")?
                    .contains(&id)
            );
            assert!(
                graph::document()
                    .lifecycle(tx, &id)?
                    .finalizers
                    .contains("factorio.session.resources")
            );
            assert!(graph::guard(tx, ROOT, "alice", false, 0, start("second")).is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn controller_observations_complete_tickets_atomically_and_release_finalizers_last() {
    use factorio::{Effect, Phase};
    let mut store = store();
    store
        .run("setup", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("one"),
                },
            )?;
            graph::command(tx, ROOT, "alice", false, 0, start("first"))?;
            graph::observe(tx, ROOT, "first", Effect::Started)?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Publish {
                    id: "first".into(),
                    evidence: "Checks passed".into(),
                    findings: vec![],
                },
            )?;
            graph::observe(
                tx,
                ROOT,
                "first",
                Effect::Published {
                    commit: "b".repeat(40),
                    target: "a".repeat(40),
                    evidence: "Checks passed".into(),
                    findings: vec![],
                },
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .run("agent approval", |tx| graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Approve {
                    id: "first".into(),
                    commit: "b".repeat(40)
                }
            ))
            .is_err()
    );
    assert!(
        store
            .run("unapproved accept", |tx| graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Accept { id: "first".into() }
            ))
            .is_err()
    );
    store
        .run("approved integration", |tx| {
            graph::command(
                tx,
                ROOT,
                "alice",
                true,
                0,
                Command::Approve {
                    id: "first".into(),
                    commit: "b".repeat(40),
                },
            )?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Accept { id: "first".into() },
            )?;
            graph::observe(
                tx,
                ROOT,
                "first",
                Effect::Integrating {
                    commit: "c".repeat(40),
                },
            )
        })
        .unwrap();
    let id = graph::child_id(ROOT, graph::SESSION_KIND, "first");
    let commit = store
        .run("integrated", |tx| {
            graph::observe(tx, ROOT, "first", Effect::Integrated)
        })
        .unwrap();
    assert_eq!(commit.value.tickets["one"].status, Status::Done);
    assert_eq!(commit.value.sessions["first"].phase, Phase::Cleanup);
    assert_eq!(
        commit
            .changes
            .iter()
            .filter(|change| change.table == "document.documents")
            .count(),
        2
    );
    store
        .inspect("cleanup owned", |tx| {
            assert!(
                graph::document()
                    .lifecycle(tx, &id)?
                    .finalizers
                    .contains("factorio.session.resources")
            );
            Ok(())
        })
        .unwrap();
    store
        .run("cleaned", |tx| {
            graph::observe(tx, ROOT, "first", Effect::Cleaned)
        })
        .unwrap();
    store
        .inspect("released", |tx| {
            assert!(graph::document().lifecycle(tx, &id)?.finalizers.is_empty());
            assert_eq!(
                graph::retained(tx, ROOT)?.sessions["first"].phase,
                Phase::Complete
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn claims_and_port_allocation_commit_atomically_across_documents() {
    let mut store = store();
    store
        .run("seed", |tx| {
            graph::onboard(tx, ROOT, "alice", config())?;
            graph::command(
                tx,
                ROOT,
                "alice",
                false,
                0,
                Command::Ticket {
                    ticket: ticket("one"),
                },
            )?;
            Ok(())
        })
        .unwrap();
    let rejected = store.run("rollback session creation", |tx| {
        graph::command(tx, ROOT, "alice", false, 0, start("first"))?;
        graph::command(tx, ROOT, "alice", false, 0, start("conflict"))
    });
    assert!(matches!(rejected, Err(Error::Constraint)));
    store
        .inspect("no partial state", |tx| {
            let state = graph::load(tx, ROOT, "alice")?;
            assert!(state.sessions.is_empty());
            assert_eq!(state.next_port, 10000);
            assert_eq!(
                graph::document().access_guard().extent(tx, "alice")?.len(),
                2
            );
            Ok(())
        })
        .unwrap();
    let committed = store
        .run("start", |tx| {
            graph::command(tx, ROOT, "alice", false, 0, start("first"))
        })
        .unwrap();
    assert_eq!(committed.value.sessions["first"].port, 10000);
    assert!(
        committed
            .changes
            .iter()
            .filter(|change| change.table == "document.documents")
            .count()
            >= 2
    );
    assert!(
        store
            .inspect("second admission", |tx| graph::guard(
                tx,
                ROOT,
                "alice",
                false,
                0,
                start("second")
            ))
            .is_err()
    );
    // Even an Owner cannot mark the ticket done through a raw named edit.
    let mut forged = ticket("one");
    forged.status = Status::Done;
    let intent = Intent {
        id: 1,
        document: graph::child_id(ROOT, graph::TICKET_KIND, "one"),
        mutation: "ticket.edit".into(),
        version: "1".into(),
        args: serde_json::to_value(forged).unwrap(),
    };
    assert!(
        store
            .inspect("forged completion", |tx| graph::document()
                .access_guard()
                .admit(tx, "alice", &intent))
            .unwrap()
            .is_err()
    );
}
