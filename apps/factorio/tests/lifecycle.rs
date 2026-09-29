use factorio::{Actor, Command, Config, Effect, Phase, Status, Ticket, Workspace};
use serde_json::json;
use snap_oidc::relying_party as rp;
use snap_store::{Error, Store};
type Database = Store<snap_sqlite::Sqlite>;
fn fixture() -> Database {
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
        rp::MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    store
        .run("fixture", |tx| {
            rp::start(
                tx,
                "fixture-state-32-characters-long",
                &rp::Attempt {
                    binding: rp::digest("alice"),
                    nonce: "nonce".into(),
                    verifier: "verifier".into(),
                    redirect: "https://factorio.test/auth/callback".into(),
                    issuer: "https://authy.test".into(),
                    old_session: None,
                    logout: false,
                    expires: 400,
                    processing: false,
                },
            )?;
            rp::consume(tx, "fixture-state-32-characters-long", "alice", false, 100)?;
            rp::issue(
                tx,
                "fixture-state-32-characters-long",
                &rp::Session {
                    id: rp::digest("alice"),
                    owner: rp::owner("https://authy.test", "alice"),
                    subject: "alice".into(),
                    issuer: "https://authy.test".into(),
                    csrf: "csrf".into(),
                    nonce: "nonce".into(),
                    profile: json!({}),
                    tokens: rp::Tokens {
                        access: "access".into(),
                        refresh: "refresh".into(),
                        id_token: "id".into(),
                        access_expires: 700,
                        auth_time: Some(100),
                    },
                    expires: 1000,
                    refreshing: false,
                    version: 1,
                },
                100,
            )?;
            factorio::initialize(
                tx,
                &Config {
                    repository: "/repo".into(),
                    mainline: "main".into(),
                    modules: [
                        ("a".into(), "crates/a".into()),
                        ("b".into(), "crates/b".into()),
                        ("c".into(), "crates/c".into()),
                    ]
                    .into_iter()
                    .collect(),
                    resources: "/resources".into(),
                    first_port: 12000,
                    setup: vec![],
                    teardown: vec![],
                },
            )
        })
        .unwrap();
    store
}
fn command(db: &mut Database, cmd: Command, human: bool) -> Result<Workspace, Error> {
    db.run("command", |tx| {
        factorio::command(
            tx,
            Actor {
                session: &rp::digest("alice"),
                human,
                now: 101,
            },
            cmd,
        )
    })
    .map(|c| c.value)
}
fn effect(db: &mut Database, id: &str, e: Effect) -> Result<Workspace, Error> {
    db.run("effect", |tx| {
        if matches!(e, Effect::Published { .. } | Effect::Integrating { .. }) {
            factorio::authorized_intent(
                tx,
                Actor {
                    session: &rp::digest("alice"),
                    human: false,
                    now: 101,
                },
                id,
                e,
            )
        } else {
            factorio::effect(tx, id, e)
        }
    })
    .map(|c| c.value)
}

#[test]
fn revocation_during_preparation_prevents_new_publication_and_integration_intent() {
    for integrating in [false, true] {
        let mut db = fixture();
        command(&mut db, start("work", &["a"], &[]), false).unwrap();
        effect(&mut db, "work", Effect::Started).unwrap();
        if integrating {
            publish(&mut db, "work", "b");
            command(
                &mut db,
                Command::Approve {
                    id: "work".into(),
                    commit: "b".repeat(40),
                },
                true,
            )
            .unwrap();
        }
        // Host has read the attempt and is outside Store preparing Git results.
        let before = view(&mut db);
        db.run("logout-during-preparation", |tx| rp::revoke(tx, "alice"))
            .unwrap();
        let prepared = if integrating {
            Effect::Integrating {
                commit: "c".repeat(40),
            }
        } else {
            Effect::Published {
                commit: "b".repeat(40),
                target: "a".repeat(40),
                evidence: "checks".into(),
                findings: vec![],
            }
        };
        assert!(effect(&mut db, "work", prepared).is_err());
        let after = view(&mut db);
        assert_eq!(after.sessions["work"].phase, before.sessions["work"].phase);
        assert!(after.sessions["work"].integration.is_none());
    }
}
fn view(db: &mut Database) -> Workspace {
    db.run("view", factorio::load).unwrap().value
}

#[test]
fn intake_batches_are_atomic_revision_guarded_and_only_single_module_leaves_become_ready() {
    use factorio::intake::{self, Drafts, Route};
    let mut db = fixture();
    let session = rp::digest("alice");
    let actor = || Actor {
        session: &session,
        human: false,
        now: 101,
    };
    db.run("create-intake", |tx| {
        intake::create(tx, actor(), "idea", "Improve navigation")
    })
    .unwrap();
    let mut parent = ticket("idea-parent", &[]);
    parent.status = Status::Draft;
    parent.modules = vec!["a".into(), "b".into()];
    let mut child = ticket("idea-child", &[]);
    child.status = Status::Draft;
    child.parent = Some(parent.id.clone());
    let mut bad = child.clone();
    bad.modules = vec!["missing".into()];
    assert!(
        db.run("bad-batch", |tx| intake::drafts(
            tx,
            actor(),
            "idea",
            Drafts {
                revision: 0,
                route: Route::Grill,
                rationale: "Need details".into(),
                tickets: vec![parent.clone(), bad]
            }
        ))
        .is_err()
    );
    assert!(view(&mut db).tickets.is_empty());
    assert_eq!(view(&mut db).intakes["idea"].revision, 0);
    db.run("good-batch", |tx| {
        intake::drafts(
            tx,
            actor(),
            "idea",
            Drafts {
                revision: 0,
                route: Route::Implement,
                rationale: "Scope agreed".into(),
                tickets: vec![parent, child.clone()],
            },
        )
    })
    .unwrap();
    assert!(
        db.run("stale", |tx| intake::drafts(
            tx,
            actor(),
            "idea",
            Drafts {
                revision: 0,
                route: Route::Explore,
                rationale: "Old reply".into(),
                tickets: vec![]
            }
        ))
        .is_err()
    );
    db.run("ready", |tx| intake::ready(tx, actor(), "idea", 1))
        .unwrap();
    let w = view(&mut db);
    assert_eq!(w.tickets["idea-parent"].status, Status::Draft);
    assert_eq!(w.tickets["idea-child"].status, Status::Ready);
    assert!(
        db.run("late-agent", |tx| intake::drafts(
            tx,
            actor(),
            "idea",
            Drafts {
                revision: 2,
                route: Route::Grill,
                rationale: "Late tool".into(),
                tickets: vec![child]
            }
        ))
        .is_err()
    );
    db.run("revoke", |tx| rp::revoke(tx, "alice")).unwrap();
    assert!(
        db.run("expired", |tx| intake::create(
            tx,
            actor(),
            "other",
            "Another request"
        ))
        .is_err()
    );
}
fn ticket(id: &str, blockers: &[&str]) -> Ticket {
    Ticket {
        id: id.into(),
        created_at: None,
        title: id.into(),
        description: String::new(),
        modules: vec!["a".into()],
        status: Status::Ready,
        notes: String::new(),
        parent: None,
        blockers: blockers.iter().map(|s| (*s).into()).collect(),
    }
}

#[test]
fn intake_readiness_preserves_settled_leaves_and_accepts_later_drafts() {
    use factorio::intake::{self, Drafts, Route};
    let mut db = fixture();
    let session = rp::digest("alice");
    let actor = || Actor {
        session: &session,
        human: false,
        now: 101,
    };
    let draft = |id: &str| {
        let mut t = ticket(id, &[]);
        t.status = Status::Draft;
        t
    };
    db.run("create", |tx| {
        intake::create(tx, actor(), "mixed", "Several small changes")
    })
    .unwrap();
    db.run("drafts", |tx| {
        intake::drafts(
            tx,
            actor(),
            "mixed",
            Drafts {
                revision: 0,
                route: Route::Implement,
                rationale: "Agreed".into(),
                tickets: vec![draft("mixed-one"), draft("mixed-two"), draft("mixed-three")],
            },
        )
    })
    .unwrap();
    let mut first = draft("mixed-one");
    first.status = Status::Ready;
    command(&mut db, Command::Ticket { ticket: first }, false).unwrap();
    let mut cancelled = draft("mixed-two");
    cancelled.status = Status::Cancelled;
    command(&mut db, Command::Ticket { ticket: cancelled }, false).unwrap();
    db.run("ready-rest", |tx| intake::ready(tx, actor(), "mixed", 3))
        .unwrap();
    let w = view(&mut db);
    assert_eq!(w.tickets["mixed-one"].status, Status::Ready);
    assert_eq!(w.tickets["mixed-two"].status, Status::Cancelled);
    assert_eq!(w.tickets["mixed-three"].status, Status::Ready);
    db.run("later-draft", |tx| {
        intake::drafts(
            tx,
            actor(),
            "mixed",
            Drafts {
                revision: 4,
                route: Route::Implement,
                rationale: "Another scoped change".into(),
                tickets: vec![draft("mixed-four")],
            },
        )
    })
    .unwrap();
    db.run("ready-new", |tx| intake::ready(tx, actor(), "mixed", 5))
        .unwrap();
    let w = view(&mut db);
    assert_eq!(w.tickets["mixed-four"].status, Status::Ready);
    assert_eq!(w.tickets["mixed-two"].status, Status::Cancelled);
    db.run("delete-intake", |tx| intake::delete(tx, actor(), "mixed"))
        .unwrap();
    let w = view(&mut db);
    assert!(!w.intakes.contains_key("mixed"));
    assert_eq!(w.tickets["mixed-four"].status, Status::Ready);
    assert!(
        db.run("retired-intake-tool", |tx| intake::drafts(
            tx,
            actor(),
            "mixed",
            Drafts {
                revision: 6,
                route: Route::Grill,
                rationale: String::new(),
                tickets: vec![draft("mixed-late")]
            }
        ))
        .is_err()
    );
}
fn start(id: &str, modules: &[&str], tickets: &[&str]) -> Command {
    Command::Start {
        id: id.into(),
        prompt: "Implement".into(),
        modules: modules.iter().map(|s| (*s).into()).collect(),
        tickets: tickets.iter().map(|s| (*s).into()).collect(),
        base: "a".repeat(40),
        conversation: format!("ses_{id}"),
    }
}
fn publish(db: &mut Database, id: &str, commit: &str) {
    effect(
        db,
        id,
        Effect::Published {
            commit: commit.repeat(40),
            target: "a".repeat(40),
            evidence: "Fixture checks passed".into(),
            findings: vec![],
        },
    )
    .unwrap();
}

#[test]
fn atomic_claims_repository_exclusion_and_restart_state() {
    let mut db = fixture();
    command(&mut db, start("one", &["a"], &[]), false).unwrap();
    assert!(command(&mut db, start("partial", &["b", "a"], &[]), false).is_err());
    command(&mut db, start("two", &["b"], &[]), false).unwrap();
    assert!(command(&mut db, start("all", &["*"], &[]), false).is_err());
    let w = view(&mut db);
    assert_eq!(w.sessions.len(), 2);
    assert_eq!(w.next_port, 12002);
    effect(&mut db, "one", Effect::Failed("setup interrupted".into())).unwrap();
    assert!(command(&mut db, start("collision", &["a"], &[]), false).is_err());
    command(&mut db, Command::Abandon { id: "one".into() }, false).unwrap();
    assert!(command(&mut db, start("collision", &["a"], &[]), false).is_err());
    effect(&mut db, "one", Effect::Cleaned).unwrap();
    command(&mut db, start("replacement", &["a"], &[]), false).unwrap();
}
#[test]
fn immutable_approval_and_atomic_completion_unblock_dependents() {
    let mut db = fixture();
    command(
        &mut db,
        Command::Ticket {
            ticket: ticket("first", &[]),
        },
        false,
    )
    .unwrap();
    command(
        &mut db,
        Command::Ticket {
            ticket: ticket("next", &["first"]),
        },
        false,
    )
    .unwrap();
    assert!(command(&mut db, start("blocked", &["a"], &["next"]), false).is_err());
    command(&mut db, start("work", &["a"], &["first"]), false).unwrap();
    effect(&mut db, "work", Effect::Started).unwrap();
    publish(&mut db, "work", "b");
    let approve = Command::Approve {
        id: "work".into(),
        commit: "b".repeat(40),
    };
    assert!(command(&mut db, approve.clone(), false).is_err());
    assert!(
        effect(
            &mut db,
            "work",
            Effect::Integrating {
                commit: "c".repeat(40)
            }
        )
        .is_err()
    );
    command(&mut db, approve, true).unwrap();
    publish(&mut db, "work", "d");
    assert_eq!(
        view(&mut db).sessions["work"].publications[0].commit,
        "b".repeat(40)
    );
    assert!(
        view(&mut db).sessions["work"]
            .candidate
            .as_ref()
            .unwrap()
            .approval
            .is_none()
    );
    assert!(
        command(
            &mut db,
            Command::Approve {
                id: "work".into(),
                commit: "b".repeat(40)
            },
            true
        )
        .is_err()
    );
    command(
        &mut db,
        Command::Approve {
            id: "work".into(),
            commit: "d".repeat(40),
        },
        true,
    )
    .unwrap();
    effect(
        &mut db,
        "work",
        Effect::Integrating {
            commit: "c".repeat(40),
        },
    )
    .unwrap();
    assert_eq!(view(&mut db).tickets["first"].status, Status::Ready);
    assert!(command(&mut db, Command::Abandon { id: "work".into() }, false).is_err());
    let w = effect(&mut db, "work", Effect::Integrated).unwrap();
    assert_eq!(w.tickets["first"].status, Status::Done);
    assert!(factorio::actionable(&w, &w.tickets["next"]));
    assert_eq!(w.sessions["work"].phase, Phase::Cleanup);
    effect(&mut db, "work", Effect::Cleaned).unwrap();
    command(&mut db, start("next-work", &["a"], &["next"]), false).unwrap();
}
#[test]
fn graph_cycles_and_completion_bypass_roll_back() {
    let mut db = fixture();
    command(
        &mut db,
        Command::Ticket {
            ticket: ticket("a", &[]),
        },
        false,
    )
    .unwrap();
    command(
        &mut db,
        Command::Ticket {
            ticket: ticket("b", &["a"]),
        },
        false,
    )
    .unwrap();
    assert!(
        command(
            &mut db,
            Command::Ticket {
                ticket: ticket("a", &["b"])
            },
            false
        )
        .is_err()
    );
    let mut a = ticket("a", &[]);
    a.parent = Some("b".into());
    command(&mut db, Command::Ticket { ticket: a }, false).unwrap();
    let mut b = ticket("b", &["a"]);
    b.parent = Some("a".into());
    assert!(command(&mut db, Command::Ticket { ticket: b }, false).is_err());
    let mut a = ticket("a", &[]);
    a.status = Status::Done;
    assert!(command(&mut db, Command::Ticket { ticket: a }, false).is_err());
    assert!(command(&mut db, Command::DeleteTicket { id: "a".into() }, false).is_err());
    assert!(view(&mut db).tickets["a"].blockers.is_empty());
}
#[test]
fn expired_authority_and_scope_expansion_are_fenced() {
    let mut db = fixture();
    command(&mut db, start("one", &["a"], &[]), false).unwrap();
    effect(&mut db, "one", Effect::Started).unwrap();
    command(&mut db, start("two", &["b"], &[]), false).unwrap();
    assert!(
        command(
            &mut db,
            Command::Expand {
                id: "one".into(),
                modules: vec!["b".into(), "c".into()]
            },
            false
        )
        .is_err()
    );
    assert_eq!(view(&mut db).sessions["one"].modules, vec!["a"]);
    command(
        &mut db,
        Command::Expand {
            id: "one".into(),
            modules: vec!["c".into()],
        },
        false,
    )
    .unwrap();
    assert!(
        db.run("expired", |tx| factorio::command(
            tx,
            Actor {
                session: &rp::digest("alice"),
                human: true,
                now: 1000
            },
            Command::Abandon { id: "one".into() }
        ))
        .is_err()
    );
}
