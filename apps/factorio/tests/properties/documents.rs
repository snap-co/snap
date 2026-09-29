use factorio::{Command, Config, Effect, Status, Ticket, documents as graph};
use hegel::{TestCase, generators as gs};
use snap_store::Error;

const ROOT: &str = "a0000000-0000-4000-8000-000000000001";

#[hegel::test]
fn linked_claims_and_cleanup_match_committed_resource_ownership(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(6));
    let actions = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(40));
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
        .run("onboard", |tx| {
            graph::onboard(
                tx,
                ROOT,
                "owner",
                Config {
                    repository: "/repo".into(),
                    resources: "/resources".into(),
                    mainline: "main".into(),
                    modules: (0..count)
                        .map(|n| (n.to_string(), format!("apps/{n}")))
                        .collect(),
                    first_port: 12000,
                    setup: vec![],
                    teardown: vec![],
                },
            )?;
            for n in 0..count {
                graph::command(
                    tx,
                    ROOT,
                    "owner",
                    false,
                    0,
                    Command::Ticket {
                        ticket: Ticket {
                            id: n.to_string(),
                            created_at: None,
                            title: format!("Module {n}"),
                            description: String::new(),
                            modules: vec![n.to_string()],
                            status: Status::Ready,
                            notes: String::new(),
                            parent: None,
                            blockers: vec![],
                        },
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    let mut claims: Vec<Option<String>> = vec![None; count];
    let mut created = 0;
    for (step, action) in actions.into_iter().enumerate() {
        let slot = action as usize % count;
        let rollback = action & 64 != 0;
        let cleanup = action & 128 != 0;
        let unauthorized = action & 32 != 0;
        let actor = if unauthorized { "stranger" } else { "owner" };
        tc.note(&format!("step={step} module={slot} rollback={rollback} cleanup={cleanup} unauthorized={unauthorized}"));
        let existing = claims[slot].clone();
        let id = format!("session-{step}");
        let result = store.run("generated composition", |tx| {
            if cleanup && let Some(existing) = &existing {
                graph::command(
                    tx,
                    ROOT,
                    actor,
                    false,
                    0,
                    Command::Abandon {
                        id: existing.clone(),
                    },
                )?;
                graph::observe(tx, ROOT, existing, Effect::Cleaned)?;
            } else {
                graph::command(
                    tx,
                    ROOT,
                    actor,
                    false,
                    0,
                    Command::Start {
                        id: id.clone(),
                        prompt: "Work".into(),
                        tickets: vec![slot.to_string()],
                        modules: vec![],
                        base: "a".repeat(40),
                        conversation: format!("ses_{step}"),
                    },
                )?;
            }
            if rollback {
                return Err(Error::Constraint);
            }
            Ok(())
        });
        let success = !unauthorized && !rollback && (existing.is_none() || cleanup);
        assert_eq!(result.is_ok(), success);
        if success {
            if cleanup && existing.is_some() {
                claims[slot] = None;
            } else {
                claims[slot] = Some(id);
                created += 1;
            }
        }
        store
            .inspect("model", |tx| {
                let state = graph::load(tx, ROOT, "owner")?;
                assert_eq!(state.next_port, 12000 + created);
                assert_eq!(state.sessions.len(), created as usize);
                for (module, claim) in claims.iter().enumerate() {
                    let live: Vec<_> = state
                        .sessions
                        .values()
                        .filter(|s| s.claims() && s.modules.contains(&module.to_string()))
                        .map(|s| s.id.clone())
                        .collect();
                    assert_eq!(live, claim.iter().cloned().collect::<Vec<_>>());
                }
                assert_eq!(
                    graph::document().cleanup_ids(tx)?.len(),
                    claims.iter().filter(|claim| claim.is_some()).count()
                );
                assert!(graph::document().authorized_ids(tx, "stranger")?.is_empty());
                Ok(())
            })
            .unwrap();
    }
}
