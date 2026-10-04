//! Generic commit/controller sequencing, without Document. Existing dispatch
//! cases own admission and rollback; these cases add post-commit participation.
#[path = "../support/host.rs"]
mod assembly;

use snap_host::{CommitContext, Participant};
use snap_platform_tests::{
    cartridge::{self, Edit, Stop},
    memory::{Memory, RejectOnce},
};
use snap_store::{Backend, Catalog, Error, Row, RowChange, Store};
use snap_transport::{Command, Event, Invocation, Response, Value, json};
use std::sync::{Arc, Mutex};

struct Controller {
    pending: bool,
    fail: bool,
    observations: Arc<Mutex<Vec<[i64; 2]>>>,
}
impl<B: Backend> Participant<B> for Controller {
    fn committed(
        &mut self,
        _: &mut CommitContext<'_, B>,
        changes: &[RowChange],
        _: &Value,
    ) -> Result<(), Error> {
        self.pending |= changes
            .iter()
            .any(|c| cartridge::TABLES.contains(&c.table.as_str()));
        Ok(())
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
    host: &mut snap_host::Blocking<B, P>,
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
fn connect<B: Backend, P: Participant<B>>(host: &mut snap_host::Blocking<B, P>) -> u64 {
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
    for fail in [false, true] {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut host = assembly::mount(store(), Default::default(), "controllers".into())
            .unwrap()
            .map_participant(|()| Controller {
                pending: false,
                fail,
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
            [Event::Completed { id: 1, outcome }] if fail => {
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
