//! One synchronous reconciliation step per pass. The shared host owns scheduling,
//! the execution gate, blocked state and explicit retries.
use factorio::{Config, Desired, Effect, Phase, Session, workspaces as graph};
use snap_document::runtime::{ControllerContext, Runtime};
use snap_store::Error;

type Context<'a> = ControllerContext<'a, snap_store_sqlite::Sqlite>;

/// Keep the effect boundary small so reconciliation can be exercised without Git
/// processes or a running OpenCode service.
trait Effects: Send + 'static {
    fn head(&mut self, config: &Config) -> Result<String, String>;
    fn setup(&mut self, config: &Config, session: &Session) -> Result<(), String>;
    fn candidate(&mut self, config: &Config, session: &Session)
    -> Result<(String, String), String>;
    fn prepare(&mut self, config: &Config, session: &Session) -> Result<String, String>;
    fn integrate(&mut self, config: &Config, session: &Session) -> Result<(), String>;
    fn cleanup(&mut self, config: &Config, session: &Session) -> Result<(), String>;
}

struct Native(tokio::runtime::Handle, crate::config::Tools);
impl Native {
    fn run<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        tokio::task::block_in_place(|| self.0.block_on(future))
    }
}
impl Effects for Native {
    fn head(&mut self, c: &Config) -> Result<String, String> {
        self.run(crate::effects::head(c))
    }
    fn setup(&mut self, c: &Config, s: &Session) -> Result<(), String> {
        self.run(crate::effects::setup(c, s, &self.1))
    }
    fn candidate(&mut self, c: &Config, s: &Session) -> Result<(String, String), String> {
        self.run(crate::effects::candidate(c, s))
    }
    fn prepare(&mut self, c: &Config, s: &Session) -> Result<String, String> {
        self.run(crate::effects::prepare(c, s))
    }
    fn integrate(&mut self, c: &Config, s: &Session) -> Result<(), String> {
        self.run(crate::effects::integrate(c, s))
    }
    fn cleanup(&mut self, c: &Config, s: &Session) -> Result<(), String> {
        self.run(crate::effects::cleanup(c, s))
    }
}

/// Native effects enter Tokio's blocking section while retaining the application
/// gate. Never wait for an agent turn under this gate.
pub fn register(
    host: Runtime<snap_store_sqlite::Sqlite>,
    runtime: tokio::runtime::Handle,
    tools: crate::config::Tools,
) -> Runtime<snap_store_sqlite::Sqlite> {
    with_effects(host, Native(runtime, tools))
}

fn with_effects(
    host: Runtime<snap_store_sqlite::Sqlite>,
    mut effects: impl Effects,
) -> Runtime<snap_store_sqlite::Sqlite> {
    host.with_controller(
        graph::SESSION_KIND,
        Box::new(move |ctx, snapshot| {
            let child: graph::Child<Session> =
                serde_json::from_value(snapshot.value).map_err(|_| Error::Invalid)?;
            let root = ctx.document(&child.workspace)?;
            let root: graph::Root =
                serde_json::from_value(root.value).map_err(|_| Error::Invalid)?;
            for id in root
                .tickets
                .values()
                .chain(root.sessions.values())
                .chain(root.intakes.values())
            {
                ctx.document(id)?;
            }
            let state = ctx.inspect("factorio.controller.inspect", |tx| {
                graph::retained(tx, &child.workspace)
            })?;
            let session = state.sessions.get(&child.data.id).ok_or(Error::NotFound)?;
            if root.sessions.get(&session.id) != Some(&snapshot.id) {
                return Err(Error::Invalid);
            }
            let lifecycle = ctx.lifecycle(&snapshot.id)?;
            if lifecycle.blocked.is_some() {
                return Ok(());
            }
            // A retained deleted/archived session still owns its resources. Finish a
            // journaled integration first; otherwise abandon while preserving work.
            if lifecycle.state != snap_document::lifecycle::State::Active
                && !matches!(
                    session.phase,
                    Phase::Integrating
                        | Phase::Cleanup
                        | Phase::Complete
                        | Phase::Abandoning
                        | Phase::Abandoned
                )
            {
                let mut session = session.clone();
                session.phase = Phase::Abandoning;
                session.desired = Desired::Abandoned;
                ctx.transact("factorio.controller.abandon", |tx| {
                    graph::document().observe(
                        tx,
                        &snapshot.id,
                        serde_json::to_value(graph::Child {
                            workspace: child.workspace,
                            data: session,
                        })
                        .map_err(|_| Error::Invalid)?,
                    )?;
                    Ok(())
                })?;
                return Ok(());
            }
            match step(&mut effects, &state.config, session) {
                Ok(Some(effect)) => {
                    publish(ctx, &child.workspace, &session.id, effect)?;
                }
                Ok(None) => {}
                Err(message) => {
                    // Persist the useful error atomically with the stop condition.
                    // Returning success avoids replacing it with a generic Store error.
                    ctx.transact("factorio.controller.blocked", |tx| {
                        graph::observe(
                            tx,
                            &child.workspace,
                            &session.id,
                            Effect::Failed(message.clone()),
                        )?;
                        let mut lifecycle = graph::document().lifecycle(tx, &snapshot.id)?;
                        lifecycle.blocked = Some(message);
                        graph::document().set_lifecycle(tx, &snapshot.id, &lifecycle)
                    })?;
                }
            }
            Ok(())
        }),
    )
}

fn publish(
    ctx: &mut Context<'_>,
    workspace: &str,
    session: &str,
    effect: Effect,
) -> Result<(), Error> {
    ctx.transact("factorio.controller.observed", |tx| {
        graph::observe(tx, workspace, session, effect)
    })?;
    Ok(())
}

fn step(
    effects: &mut impl Effects,
    config: &Config,
    session: &Session,
) -> Result<Option<Effect>, String> {
    Ok(Some(match session.phase {
        Phase::Starting if session.base.is_empty() => Effect::Based(effects.head(config)?),
        Phase::Starting => {
            effects.setup(config, session)?;
            Effect::Started
        }
        Phase::Active => match &session.desired {
            Desired::Published { evidence, findings } => {
                let (commit, target) = effects.candidate(config, session)?;
                Effect::Published {
                    commit,
                    target,
                    evidence: evidence.clone(),
                    findings: findings.clone(),
                }
            }
            _ => return Ok(None),
        },
        Phase::Published if matches!(session.desired, Desired::Integrated) => {
            let candidate = session.candidate.as_ref().ok_or("Missing candidate")?;
            if candidate
                .approval
                .as_ref()
                .is_none_or(|approval| approval.commit != candidate.commit)
            {
                return Err("Awaiting human approval of this candidate".into());
            }
            // The next pass moves mainline only after this exact OID commits.
            Effect::Integrating {
                commit: effects.prepare(config, session)?,
            }
        }
        Phase::Integrating => {
            effects.integrate(config, session)?;
            Effect::Integrated
        }
        Phase::Cleanup | Phase::Abandoning => {
            effects.cleanup(config, session)?;
            Effect::Cleaned
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use factorio::{Command, Status, Ticket};
    use std::sync::{Arc, Mutex};

    const ROOT: &str = "a0000000-0000-4000-8000-000000000001";
    #[derive(Default)]
    struct Calls {
        log: Vec<&'static str>,
        fail: Option<&'static str>,
        integrated: bool,
    }
    struct Fake(Arc<Mutex<Calls>>);
    impl Fake {
        fn call(&self, name: &'static str) -> Result<(), String> {
            let mut calls = self.0.lock().unwrap();
            calls.log.push(name);
            if calls.fail == Some(name) {
                Err(format!("{name} failed"))
            } else {
                Ok(())
            }
        }
    }
    impl Effects for Fake {
        fn head(&mut self, _: &Config) -> Result<String, String> {
            self.call("head")?;
            Ok("a".repeat(40))
        }
        fn setup(&mut self, _: &Config, _: &Session) -> Result<(), String> {
            self.call("setup")
        }
        fn candidate(&mut self, _: &Config, _: &Session) -> Result<(String, String), String> {
            self.call("candidate")?;
            Ok(("b".repeat(40), "a".repeat(40)))
        }
        fn prepare(&mut self, _: &Config, _: &Session) -> Result<String, String> {
            self.call("prepare")?;
            Ok("c".repeat(40))
        }
        fn integrate(&mut self, _: &Config, session: &Session) -> Result<(), String> {
            assert_eq!(
                session.integration.as_deref(),
                Some("c".repeat(40).as_str())
            );
            // Simulate an external success whose acknowledgement was lost.
            self.0.lock().unwrap().integrated = true;
            self.call("integrate")
        }
        fn cleanup(&mut self, _: &Config, _: &Session) -> Result<(), String> {
            self.call("cleanup")
        }
    }
    fn fixture() -> (Runtime<snap_store_sqlite::Sqlite>, Arc<Mutex<Calls>>) {
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
            .run("seed", |tx| {
                graph::onboard(
                    tx,
                    ROOT,
                    "owner",
                    Config {
                        repository: "/repo".into(),
                        resources: "/resources".into(),
                        mainline: "main".into(),
                        modules: [("one".into(), "apps/one".into())].into_iter().collect(),
                        first_port: 12000,
                        setup: vec![],
                        teardown: vec![],
                    },
                )?;
                graph::command(
                    tx,
                    ROOT,
                    "owner",
                    false,
                    0,
                    Command::Ticket {
                        ticket: Ticket {
                            id: "ticket".into(),
                            created_at: None,
                            title: "Change".into(),
                            description: String::new(),
                            modules: vec!["one".into()],
                            status: Status::Ready,
                            notes: String::new(),
                            parent: None,
                            blockers: vec![],
                        },
                    },
                )?;
                graph::command(
                    tx,
                    ROOT,
                    "owner",
                    false,
                    0,
                    Command::Start {
                        id: "work".into(),
                        prompt: "Implement".into(),
                        tickets: vec!["ticket".into()],
                        modules: vec![],
                        base: "a".repeat(40),
                        conversation: "ses_work".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let host = Runtime::new(
            store,
            graph::document(),
            Arc::new(|_, bearer| Ok(bearer.into())),
            snap_transport::server::Config::default(),
            "controller-tests".into(),
        );
        (with_effects(host, Fake(calls.clone())), calls)
    }
    fn command(host: &mut Runtime<snap_store_sqlite::Sqlite>, cmd: Command, human: bool) {
        host.transact("command", |tx| {
            graph::command(tx, ROOT, "owner", human, 0, cmd)
        })
        .unwrap();
    }
    fn retry(host: &mut Runtime<snap_store_sqlite::Sqlite>) {
        host.transact("retry", |tx| {
            let id = graph::child_id(ROOT, graph::SESSION_KIND, "work");
            let mut lifecycle = graph::document().lifecycle(tx, &id)?;
            lifecycle.blocked = None;
            graph::document().set_lifecycle(tx, &id, &lifecycle)
        })
        .unwrap();
    }

    #[test]
    fn startup_converges_and_unknown_integration_outcome_retries_recorded_commit() {
        let (mut host, calls) = fixture();
        host.recover_controllers().unwrap();
        command(
            &mut host,
            Command::Publish {
                id: "work".into(),
                evidence: "checked".into(),
                findings: vec![],
            },
            false,
        );
        command(
            &mut host,
            Command::Approve {
                id: "work".into(),
                commit: "b".repeat(40),
            },
            true,
        );
        calls.lock().unwrap().fail = Some("integrate");
        command(&mut host, Command::Accept { id: "work".into() }, false);
        host.transact("blocked inspection", |tx| {
            let state = graph::retained(tx, ROOT)?;
            assert_eq!(state.sessions["work"].phase, Phase::Integrating);
            assert_eq!(state.tickets["ticket"].status, Status::Ready);
            assert_eq!(state.sessions["work"].error, "integrate failed");
            Ok(())
        })
        .unwrap();
        host.recover_controllers().unwrap();
        assert_eq!(
            calls.lock().unwrap().log,
            ["setup", "candidate", "prepare", "integrate"]
        );
        calls.lock().unwrap().fail = None;
        retry(&mut host);
        host.transact("complete", |tx| {
            let state = graph::retained(tx, ROOT)?;
            assert_eq!(state.sessions["work"].phase, Phase::Complete);
            assert_eq!(state.tickets["ticket"].status, Status::Done);
            assert!(graph::document().cleanup_ids(tx)?.is_empty());
            Ok(())
        })
        .unwrap();
        host.recover_controllers().unwrap();
        assert_eq!(
            calls.lock().unwrap().log,
            [
                "setup",
                "candidate",
                "prepare",
                "integrate",
                "integrate",
                "cleanup"
            ]
        );
    }

    #[test]
    fn setup_failure_waits_for_explicit_retry_and_deletion_runs_cleanup() {
        let (mut host, calls) = fixture();
        calls.lock().unwrap().fail = Some("setup");
        host.recover_controllers().unwrap();
        host.recover_controllers().unwrap();
        assert_eq!(calls.lock().unwrap().log, ["setup"]);
        calls.lock().unwrap().fail = None;
        retry(&mut host);
        calls.lock().unwrap().fail = Some("cleanup");
        let id = graph::child_id(ROOT, graph::SESSION_KIND, "work");
        host.transact("delete", |tx| graph::document().remove(tx, &id, "owner"))
            .unwrap();
        host.transact("retained", |tx| {
            assert!(graph::document().cleanup_ids(tx)?.contains(&id));
            assert_eq!(
                graph::retained(tx, ROOT)?.sessions["work"].phase,
                Phase::Abandoning
            );
            Ok(())
        })
        .unwrap();
        calls.lock().unwrap().fail = None;
        retry(&mut host);
        host.transact("abandoned", |tx| {
            assert!(graph::document().cleanup_ids(tx)?.is_empty());
            assert_eq!(
                graph::retained(tx, ROOT)?.sessions["work"].phase,
                Phase::Abandoned
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(
            calls.lock().unwrap().log,
            ["setup", "setup", "cleanup", "cleanup"]
        );
    }
}
