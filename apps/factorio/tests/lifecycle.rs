use factorio::{Command, Config, Effect, Phase, Status, Ticket, Workspace, workspaces as graph};
use snap_store::{Error, Store};
type Database = Store<snap_store_sqlite::Sqlite>;
const ROOT: &str = "a0000000-0000-4000-8000-000000000001";
fn fixture() -> Database {
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    store
        .run("fixture", |tx| {
            graph::onboard(
                tx,
                ROOT,
                "alice",
                Config {
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
        graph::command(tx, ROOT, "alice", human, 101, cmd)
    })
    .map(|c| c.value)
}
fn effect(db: &mut Database, id: &str, e: Effect) -> Result<Workspace, Error> {
    db.run("effect", |tx| graph::observe(tx, ROOT, id, e))
        .map(|c| c.value)
}

fn view(db: &mut Database) -> Workspace {
    db.inspect("view", |tx| graph::load(tx, ROOT, "alice"))
        .unwrap()
}

#[test]
fn intake_readiness_promotes_only_single_module_leaves_and_fences_late_drafts() {
    use factorio::intake::{Drafts, Route};
    let mut db = fixture();
    db.run("create-intake", |tx| {
        factorio::intake::create(tx, ROOT, "alice", "idea", "Improve navigation")
    })
    .unwrap();
    let mut parent = ticket("idea-parent", &[]);
    parent.status = Status::Draft;
    parent.modules = vec!["a".into(), "b".into()];
    let mut child = ticket("idea-child", &[]);
    child.status = Status::Draft;
    child.parent = Some(parent.id.clone());
    let mut multi = child.clone();
    multi.id = "idea-zmulti".into();
    multi.modules = vec!["a".into(), "b".into()];
    db.run("good-batch", |tx| {
        factorio::intake::drafts(
            tx,
            ROOT,
            "alice",
            "idea",
            Drafts {
                revision: 0,
                route: Route::Implement,
                rationale: "Scope agreed".into(),
                tickets: vec![parent, child.clone(), multi.clone()],
            },
            101,
        )
    })
    .unwrap();
    assert!(
        db.run("multi-module leaf", |tx| factorio::intake::ready(
            tx, ROOT, "alice", "idea", 1
        ))
        .is_err()
    );
    let w = view(&mut db);
    assert_eq!(w.tickets["idea-child"].status, Status::Draft);
    assert_eq!(w.intakes["idea"].revision, 1);
    multi.modules = vec!["b".into()];
    command(&mut db, Command::Ticket { ticket: multi }, false).unwrap();
    db.run("ready", |tx| {
        factorio::intake::ready(tx, ROOT, "alice", "idea", 2)
    })
    .unwrap();
    let w = view(&mut db);
    assert_eq!(w.tickets["idea-parent"].status, Status::Draft);
    assert_eq!(w.tickets["idea-child"].status, Status::Ready);
    assert_eq!(w.tickets["idea-zmulti"].status, Status::Ready);
    assert!(
        db.run("late-agent", |tx| factorio::intake::drafts(
            tx,
            ROOT,
            "alice",
            "idea",
            Drafts {
                revision: 3,
                route: Route::Grill,
                rationale: "Late tool".into(),
                tickets: vec![child]
            },
            101,
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
    use factorio::intake::{Drafts, Route};
    let mut db = fixture();
    let draft = |id: &str| {
        let mut t = ticket(id, &[]);
        t.status = Status::Draft;
        t
    };
    db.run("create", |tx| {
        factorio::intake::create(tx, ROOT, "alice", "mixed", "Several small changes")
    })
    .unwrap();
    db.run("drafts", |tx| {
        factorio::intake::drafts(
            tx,
            ROOT,
            "alice",
            "mixed",
            Drafts {
                revision: 0,
                route: Route::Implement,
                rationale: "Agreed".into(),
                tickets: vec![draft("mixed-one"), draft("mixed-two"), draft("mixed-three")],
            },
            101,
        )
    })
    .unwrap();
    let mut first = draft("mixed-one");
    first.status = Status::Ready;
    command(&mut db, Command::Ticket { ticket: first }, false).unwrap();
    let mut cancelled = draft("mixed-two");
    cancelled.status = Status::Cancelled;
    command(&mut db, Command::Ticket { ticket: cancelled }, false).unwrap();
    db.run("ready-rest", |tx| {
        factorio::intake::ready(tx, ROOT, "alice", "mixed", 3)
    })
    .unwrap();
    let w = view(&mut db);
    assert_eq!(w.tickets["mixed-one"].status, Status::Ready);
    assert_eq!(w.tickets["mixed-two"].status, Status::Cancelled);
    assert_eq!(w.tickets["mixed-three"].status, Status::Ready);
    db.run("later-draft", |tx| {
        factorio::intake::drafts(
            tx,
            ROOT,
            "alice",
            "mixed",
            Drafts {
                revision: 4,
                route: Route::Implement,
                rationale: "Another scoped change".into(),
                tickets: vec![draft("mixed-four")],
            },
            101,
        )
    })
    .unwrap();
    db.run("ready-new", |tx| {
        factorio::intake::ready(tx, ROOT, "alice", "mixed", 5)
    })
    .unwrap();
    let w = view(&mut db);
    assert_eq!(w.tickets["mixed-four"].status, Status::Ready);
    assert_eq!(w.tickets["mixed-two"].status, Status::Cancelled);
    db.run("delete-intake", |tx| {
        factorio::intake::delete(tx, ROOT, "alice", "mixed")
    })
    .unwrap();
    let w = view(&mut db);
    assert!(!w.intakes.contains_key("mixed"));
    assert_eq!(w.tickets["mixed-four"].status, Status::Ready);
    assert!(
        db.run("retired-intake-tool", |tx| factorio::intake::drafts(
            tx,
            ROOT,
            "alice",
            "mixed",
            Drafts {
                revision: 6,
                route: Route::Grill,
                rationale: String::new(),
                tickets: vec![draft("mixed-late")]
            },
            101,
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
fn repository_claims_survive_failures_and_release_only_after_cleanup() {
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
fn scope_expansion_is_atomic_and_excludes_other_sessions() {
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
    assert_eq!(view(&mut db).sessions["one"].modules, vec!["a", "c"]);
}
