//! Generic commit/controller sequencing, without Document. Existing dispatch
//! cases own admission and rollback; these cases add post-commit participation.
#[path = "../support/host.rs"]
mod assembly;

use snap_platform_tests::{
    cartridge::{self, Edit, Stop},
    memory::{Memory, RejectOnce},
};
use snap_store::{Backend, Catalog, Error, Row, RowChange, Store};
use snap_transport::host::{CommitContext, Participant};
use snap_transport::{Command, Event, Invocation, Response, Value, json};
use std::sync::{Arc, Mutex};

struct Controller {
    pending: bool,
    fail: bool,
    fail_notification: bool,
    observations: Arc<Mutex<Vec<[i64; 2]>>>,
}
impl<B: Backend> Participant<B> for Controller {
    fn prepare(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        snap_store::Data::new(&cartridge::TABLES).prepare(ctx.store)
    }
    fn committed(
        &mut self,
        _: &mut CommitContext<'_, B>,
        changes: &[RowChange],
        _: &Value,
    ) -> Result<(), Error> {
        let changed = changes
            .iter()
            .any(|c| cartridge::TABLES.contains(&c.table.as_str()));
        self.pending |= changed;
        if changed && self.fail_notification {
            Err(Error::Unavailable)
        } else {
            Ok(())
        }
    }
    fn reconcile(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<bool, Error> {
        if !core::mem::take(&mut self.pending) {
            return Ok(false);
        }
        let observed = ctx.store.inspect("controller.observe", cartridge::read)?;
        self.observations.lock().unwrap().push(observed);
        if let Some(invocation) = &ctx.invocation {
            invocation.progress.send(json!(observed))?;
        }
        if observed == [1; 2] {
            ctx.transact("controller.advance", |tx| {
                for table in cartridge::TABLES {
                    tx.update(table, &[1.into()], Row::from([("value".into(), 2.into())]))?;
                }
                Ok(())
            })?;
        }
        if self.fail {
            Err(Error::Unavailable)
        } else {
            Ok(true)
        }
    }
}
fn call(id: u64, expected: i64, amount: i64, stop: Stop) -> Command {
    Command::Invoke(Invocation {
        id,
        operation: "probe.change".into(),
        input: serde_json::to_value(Edit {
            expected,
            amount,
            stop,
        })
        .unwrap(),
    })
}
fn events<B: Backend, P: Participant<B>>(
    host: &mut snap_transport::host::Blocking<B, P>,
    peer: u64,
) -> Vec<Event> {
    host.drain(peer)
        .unwrap()
        .into_iter()
        .map(|r| match r {
            Response::Event(e) => e,
            other => panic!("unexpected response: {other:?}"),
        })
        .collect()
}
fn store() -> Store<Memory> {
    let catalog = catalog();
    Store::new(catalog.clone(), Memory::new(catalog).unwrap()).unwrap()
}
fn catalog() -> Catalog {
    assembly::migrations()
        .iter()
        .try_fold(Catalog::default(), |catalog, migration| {
            migration.apply(&catalog)
        })
        .unwrap()
}
fn connect<B: Backend, P: Participant<B>>(host: &mut snap_transport::host::Blocking<B, P>) -> u64 {
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: "alice".into(),
            client_id: "controllers".into(),
        },
        0,
    )
    .unwrap();
    assert_eq!(
        host.drain(peer).unwrap(),
        [Response::Attached { resumed: false }]
    );
    peer
}

#[test]
fn controller_commits_notify_again_before_completion_and_next_admission_even_on_failure() {
    for (fail, fail_notification) in [(false, false), (true, false), (false, true)] {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut host = assembly::mount(store(), Default::default(), "controllers".into())
            .unwrap()
            .map_participant(|()| Controller {
                pending: false,
                fail,
                fail_notification,
                observations: observations.clone(),
            });
        let peer = connect(&mut host);
        host.submit(peer, call(1, 0, 1, Stop::Commit), 0).unwrap();
        host.submit(peer, call(2, 2, 10, Stop::Commit), 0).unwrap();
        assert_eq!(events(&mut host, peer), [Event::Accepted { id: 1 }]);
        assert!(host.step());
        assert_eq!(*observations.lock().unwrap(), [[1; 2], [2; 2]]);
        let first = events(&mut host, peer);
        assert_eq!(
            &first[..2],
            [
                Event::Progress {
                    id: 1,
                    value: json!([1, 1])
                },
                Event::Progress {
                    id: 1,
                    value: json!([2, 2])
                }
            ]
        );
        match &first[2..] {
            [Event::Completed { id: 1, outcome }] if fail || fail_notification => {
                assert!(
                    matches!(outcome, Err(snap_transport::Error::Application(v)) if v["committed"] == true)
                );
            }
            [Event::Completed { id: 1, outcome }] => assert_eq!(outcome, &Ok(json!([1, 1]))),
            other => panic!("unexpected completion: {other:?}"),
        }
        assert!(host.step());
        let second = events(&mut host, peer);
        assert!(matches!(second.first(), Some(Event::Accepted { id: 2 })));
        assert_eq!(*observations.lock().unwrap(), [[1; 2], [2; 2], [12; 2]]);
        assert!(!host.step());
        assert_eq!(
            host.transact("persisted", cartridge::read).unwrap(),
            [12; 2]
        );
    }
}

#[test]
fn discarded_and_rejected_commits_never_notify_controllers() {
    for stop in [
        Stop::Application,
        Stop::InvalidOutput,
        Stop::CaughtMiss,
        Stop::Commit,
    ] {
        let catalog = catalog();
        let (backend, rejection) = RejectOnce::new(Memory::new(catalog.clone()).unwrap());
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut host = assembly::mount(
            Store::new(catalog, backend).unwrap(),
            Default::default(),
            "failures".into(),
        )
        .unwrap()
        .map_participant(|()| Controller {
            pending: false,
            fail: false,
            fail_notification: false,
            observations: observations.clone(),
        });
        let peer = connect(&mut host);
        if stop == Stop::Commit {
            rejection.arm();
        }
        host.submit(peer, call(1, 0, 99, stop), 0).unwrap();
        assert_eq!(events(&mut host, peer), [Event::Accepted { id: 1 }]);
        assert!(host.step());
        assert!(matches!(
            events(&mut host, peer).as_slice(),
            [Event::Completed {
                id: 1,
                outcome: Err(_)
            }]
        ));
        assert!(observations.lock().unwrap().is_empty());
        assert_eq!(host.transact("persisted", cartridge::read).unwrap(), [0; 2]);
    }
}

#[test]
fn internal_notification_failure_drains_controller_commits_before_returning() {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut host = assembly::mount(store(), Default::default(), "internal".into())
        .unwrap()
        .map_participant(|()| Controller {
            pending: false,
            fail: false,
            fail_notification: true,
            observations: observations.clone(),
        });
    let peer = connect(&mut host);
    let result = host.transact("internal.write", |tx| {
        for table in cartridge::TABLES {
            tx.update(table, &[1.into()], Row::from([("value".into(), 1.into())]))?;
        }
        Ok(())
    });
    assert_eq!(result, Err(Error::Unavailable));
    assert_eq!(*observations.lock().unwrap(), [[1; 2], [2; 2]]);
    assert!(events(&mut host, peer).is_empty());
    host.submit(peer, call(2, 2, 10, Stop::Commit), 0).unwrap();
    assert_eq!(events(&mut host, peer), [Event::Accepted { id: 2 }]);
    assert!(host.step());
    assert_eq!(*observations.lock().unwrap(), [[1; 2], [2; 2], [12; 2]]);
    assert_eq!(
        host.transact("persisted", cartridge::read).unwrap(),
        [12; 2]
    );
}

#[test]
fn staged_completion_commits_after_controller_work_and_never_publishes_failed_credentials() {
    use snap_transport::{
        Operation,
        bearer::{Change, Receiver, Token},
        host::Blocking,
        operation::{Definition, Guard, Registry},
    };
    struct Acquire;
    impl Operation for Acquire {
        const NAME: &'static str = "probe.acquire";
        type Input = i64;
        type Output = i64;
        type Error = ();
        type Progress = ();
    }
    // Invalid output and rejected persistence are distinct failure boundaries.
    for failure in ["none", "output", "commit", "controller", "begin"] {
        let catalog = catalog();
        let (backend, rejection) = RejectOnce::new(Memory::new(catalog.clone()).unwrap());
        let mut store = Store::new(catalog, backend).unwrap();
        store
            .run("seed", |tx| {
                for table in cartridge::TABLES {
                    tx.insert(
                        table,
                        Row::from([("id".into(), 1.into()), ("value".into(), 0.into())]),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        struct Advance {
            pending: bool,
            rejection: snap_platform_tests::memory::CommitRejection,
            failure: &'static str,
            output: Option<snap_transport::runtime::Output>,
        }
        impl<B: Backend> Participant<B> for Advance {
            fn committed(
                &mut self,
                _: &mut CommitContext<'_, B>,
                changes: &[RowChange],
                _: &Value,
            ) -> Result<(), Error> {
                self.pending |= changes
                    .iter()
                    .any(|change| change.table == cartridge::TABLES[0]);
                Ok(())
            }
            fn reconcile(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<bool, Error> {
                if !core::mem::take(&mut self.pending) {
                    return Ok(false);
                }
                let current = ctx.store.inspect("observe", cartridge::read)?;
                assert!(
                    self.output.as_ref().unwrap().is_empty(),
                    "publication precedes reconciliation"
                );
                if current[0] == 1 {
                    ctx.transact("effect.complete", |tx| {
                        tx.update(
                            cartridge::TABLES[0],
                            &[1.into()],
                            Row::from([("value".into(), 2.into())]),
                        )
                    })?;
                    if self.failure == "commit" {
                        self.rejection.arm();
                    }
                    if self.failure == "controller" {
                        return Err(Error::Unavailable);
                    }
                }
                if current[0] == 3 {
                    ctx.transact("completion.observed", |tx| {
                        tx.update(
                            cartridge::TABLES[1],
                            &[1.into()],
                            Row::from([("value".into(), 4.into())]),
                        )
                    })?;
                }
                Ok(true)
            }
        }
        let mut acquire = Definition::staged::<Acquire, i64>(
            false,
            vec![Guard::new(|tx, call, _| {
                if cartridge::read(tx)?[0] != call.input.as_i64().unwrap() {
                    return Err(Error::Constraint.into());
                }
                Ok(())
            })],
            snap_store::Data::new(&cartridge::TABLES),
            &[],
            move |tx, _, context| {
                tx.update(
                    cartridge::TABLES[0],
                    &[1.into()],
                    Row::from([("value".into(), 1.into())]),
                )?;
                context
                    .bearer_changed(Change::Set(Token::new("not-yet-issued".into())))
                    .unwrap();
                if failure == "begin" {
                    return Err(Error::Unavailable.into());
                }
                Ok(1)
            },
            |tx, key, context| {
                let row = tx.get(cartridge::TABLES[0], &[key.into()])?.unwrap();
                if row["value"] != 2.into() {
                    return Err(Error::Unavailable.into());
                }
                tx.update(
                    cartridge::TABLES[0],
                    &[key.into()],
                    Row::from([("value".into(), 3.into())]),
                )?;
                context
                    .bearer_changed(Change::Set(Token::new("issued".into())))
                    .unwrap();
                Ok(3)
            },
        );
        if failure == "output" {
            acquire.output = |_| false;
        }
        let mut host = Blocking::new(
            store,
            Advance {
                pending: false,
                rejection,
                failure,
                output: None,
            },
            Registry::default().with_request(acquire),
            Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
                |_, bearer| Ok(bearer.into()),
            ))),
            Default::default(),
            "staged".into(),
        );
        let peer = host.open().unwrap();
        let output = host.output(peer).unwrap();
        let mut host = host.map_participant(|mut participant| {
            participant.output = Some(output);
            participant
        });
        host.submit(
            peer,
            Command::Request {
                bearer: None,
                invocation: Invocation {
                    id: 9,
                    operation: Acquire::NAME.into(),
                    input: json!(0),
                },
            },
            0,
        )
        .unwrap();
        assert_eq!(events(&mut host, peer), [Event::Accepted { id: 9 }]);
        assert!(host.step());
        let result = events(&mut host, peer);
        if failure == "none" {
            assert_eq!(
                result,
                [
                    Event::Bearer {
                        id: 9,
                        change: Change::Set(Token::new("issued".into()))
                    },
                    Event::Completed {
                        id: 9,
                        outcome: Ok(json!(3))
                    }
                ]
            );
        } else {
            assert!(matches!(
                result.as_slice(),
                [Event::Completed {
                    id: 9,
                    outcome: Err(_)
                }]
            ));
        }
        let expected = match failure {
            "none" => 3,
            "begin" => 0,
            _ => 2,
        };
        assert_eq!(
            host.transact("persisted", cartridge::read).unwrap(),
            [expected, if failure == "none" { 4 } else { 0 }]
        );
    }
}
