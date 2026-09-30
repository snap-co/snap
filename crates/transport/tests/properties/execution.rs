#[path = "execution_fixture.rs"]
mod fixture;

use fixture::{Ledger, call, state};
use hegel::{TestCase, generators as gs};
use snap_transport::execution::*;
use std::collections::BTreeMap;

fn assert_records(host: &Executor<Ledger>, histories: &[Vec<i64>; 3]) {
    for (i, history) in histories.iter().enumerate() {
        assert_eq!(host.state(Scope(i as u64)), Some(&state(history)));
    }
}

#[derive(Debug)]
struct Plan {
    scope: Option<Scope>,
    delta: i64,
    reads: usize,
    mode: u8,
    fail_read: Option<usize>,
}

#[hegel::test]
fn queued_attempts_publish_only_complete_valid_proposals(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(24));
    let mut host = Executor::new(Ledger::default(), count).unwrap();
    for i in 0..3 {
        host.open(Scope(i)).unwrap();
    }
    let mut plans = Vec::new();
    let mut tickets = Vec::new();
    for _ in 0..count {
        let scope = tc.draw(gs::integers::<u64>().max_value(3));
        let reads = tc.draw(gs::integers::<usize>().max_value(3));
        let plan = Plan {
            scope: (scope != 3).then_some(Scope(scope)),
            delta: tc.draw(gs::integers::<i64>().min_value(-20).max_value(20)),
            reads,
            mode: tc.draw(gs::integers::<u8>().max_value(4)),
            fail_read: if reads > 0 && tc.draw(gs::booleans()) {
                Some(tc.draw(gs::integers::<usize>().max_value(reads - 1)))
            } else {
                None
            },
        };
        tickets.push(
            host.submit(plan.scope, call(plan.delta, plan.reads, plan.mode, false))
                .unwrap(),
        );
        plans.push(plan);
    }
    tc.note(&format!("plans={plans:?}"));
    assert_eq!(
        host.submit(None, call(0, 0, 0, false)),
        Err(Error::Capacity)
    );
    let closing = tc.draw(gs::integers::<u64>().max_value(2));
    host.release(Scope(closing));
    host.release(Scope(closing));
    assert_eq!(
        host.submit(Some(Scope(closing)), call(0, 0, 0, false)),
        Err(Error::Protocol)
    );
    let mut histories: [Vec<i64>; 3] = Default::default();
    for (index, plan) in plans.iter().enumerate() {
        let ticket = tickets[index];
        assert_eq!(host.step(), Some(Event::Accepted(ticket)));
        assert_records(&host, &histories);
        let mut failed = false;
        let requests = if plan.mode == 4 {
            plan.reads.max(1)
        } else {
            plan.reads
        };
        for read in 0..requests {
            let key = format!("input-{read}");
            assert_eq!(
                host.step(),
                Some(Event::Need {
                    ticket,
                    key: key.clone()
                })
            );
            assert_records(&host, &histories);
            let waits = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
            for _ in 0..waits {
                assert_eq!(
                    host.step(),
                    None,
                    "waiting work owns the whole application gate"
                );
                assert_records(&host, &histories);
            }
            let wrong = tickets[(index + 1) % count];
            if wrong != ticket {
                assert_eq!(host.supply(wrong, &key, Ok(json!(0))), Err(Error::Protocol));
            }
            assert_eq!(
                host.supply(ticket, "wrong-key", Ok(json!(0))),
                Err(Error::Protocol)
            );
            if tc.draw(gs::booleans()) {
                host.pause();
                assert_eq!(
                    host.submit(None, call(0, 0, 0, false)),
                    Err(Error::Unavailable)
                );
                assert_eq!(host.restore(&empty_snapshot()), Err(Error::Unavailable));
                assert_eq!(host.replace(Ledger::default()), Err(Error::Unavailable));
                host.resume();
            }
            failed = plan.fail_read == Some(read);
            host.supply(
                ticket,
                &key,
                if failed {
                    Err(Error::Unavailable)
                } else {
                    Ok(json!(tc.draw(gs::integers::<i64>())))
                },
            )
            .unwrap();
            assert_eq!(
                host.supply(ticket, &key, Ok(json!(0))),
                Err(Error::Protocol),
                "duplicate reply must not resume twice"
            );
            assert_records(&host, &histories);
            if failed {
                break;
            }
        }
        let outcome = if failed {
            Err(Error::Unavailable)
        } else {
            match plan.mode {
                1 => Err(Error::Application(json!("denied"))),
                2 => Err(Error::InvalidOutput),
                3 => Err(Error::InvalidState),
                4 => Err(Error::Protocol),
                _ => {
                    let sum = if let Some(scope) = plan.scope {
                        histories[scope.0 as usize].push(plan.delta);
                        histories[scope.0 as usize].iter().sum::<i64>()
                    } else {
                        plan.delta
                    };
                    Ok(json!(sum))
                }
            }
        };
        assert_eq!(host.step(), Some(Event::Completed { ticket, outcome }));
        tc.event(format!("mode={} dependency_failed={failed}", plan.mode));
        assert_records(&host, &histories);
        assert_eq!(
            host.supply(ticket, "input-0", Ok(json!(0))),
            Err(Error::Protocol)
        );
    }
    assert_eq!(host.step(), None);
    assert!(host.idle());
    assert_eq!(host.state(Scope(closing)), None);
    host.open(Scope(closing)).unwrap();
    assert_eq!(host.state(Scope(closing)), Some(&Value::Null));
}

fn empty_snapshot() -> Snapshot {
    Executor::new(Ledger::default(), 1)
        .unwrap()
        .snapshot()
        .unwrap()
}

#[hegel::test]
fn admission_and_discovery_limits_preserve_committed_state(tc: TestCase) {
    // Directed sizes make both sides of the 64-input bound unavoidable.
    let reads = [0, 1, 63, 64, 65][tc.draw(gs::integers::<usize>().max_value(4))];
    let guard = tc.draw(gs::booleans());
    let allowed = tc.draw(gs::booleans());
    let delta = tc.draw(gs::integers::<i64>().min_value(-100).max_value(100));
    let mut host = Executor::new(Ledger::default(), 1).unwrap();
    host.open(Scope(0)).unwrap();
    let ticket = host
        .submit(Some(Scope(0)), call(delta, reads, 0, guard))
        .unwrap();
    if guard {
        assert_eq!(
            host.step(),
            Some(Event::Need {
                ticket,
                key: "guard".into()
            })
        );
        assert_eq!(host.state(Scope(0)), Some(&Value::Null));
        host.supply(ticket, "guard", Ok(json!(allowed))).unwrap();
        if !allowed {
            assert_eq!(
                host.step(),
                Some(Event::Completed {
                    ticket,
                    outcome: Err(Error::Application(json!("denied")))
                })
            );
            assert_eq!(host.state(Scope(0)), Some(&Value::Null));
            assert!(host.idle());
            return;
        }
    }
    assert_eq!(host.step(), Some(Event::Accepted(ticket)));
    let available = 64 - usize::from(guard);
    for index in 0..reads.min(available) {
        let key = format!("input-{index}");
        assert_eq!(
            host.step(),
            Some(Event::Need {
                ticket,
                key: key.clone()
            })
        );
        assert_eq!(host.state(Scope(0)), Some(&Value::Null));
        host.supply(ticket, &key, Ok(json!(index))).unwrap();
    }
    let outcome = if reads > available {
        Err(Error::Capacity)
    } else {
        Ok(json!(delta))
    };
    assert_eq!(host.step(), Some(Event::Completed { ticket, outcome }));
    assert_eq!(
        host.state(Scope(0)),
        Some(&if reads > available {
            Value::Null
        } else {
            state(&[delta])
        })
    );
    assert!(host.idle());
}

fn apply(host: &mut Executor<Ledger>, scope: Scope, delta: i64) -> Value {
    let ticket = host.submit(Some(scope), call(delta, 1, 0, false)).unwrap();
    assert_eq!(host.step(), Some(Event::Accepted(ticket)));
    assert_eq!(
        host.step(),
        Some(Event::Need {
            ticket,
            key: "input-0".into()
        })
    );
    host.supply(ticket, "input-0", Ok(json!(7))).unwrap();
    let Some(Event::Completed {
        ticket: done,
        outcome: Ok(value),
    }) = host.step()
    else {
        panic!("expected successful replay")
    };
    assert_eq!(ticket, done);
    value
}

#[hegel::test]
fn snapshots_replay_captured_inputs_and_replacement_changes_only_code(tc: TestCase) {
    let history =
        tc.draw(gs::vecs(gs::integers::<i64>().min_value(-100).max_value(100)).max_size(20));
    let replay = tc.draw(
        gs::vecs(gs::integers::<i64>().min_value(-100).max_value(100))
            .min_size(1)
            .max_size(20),
    );
    let mut host = Executor::new(Ledger::default(), 2).unwrap();
    host.open(Scope(0)).unwrap();
    for delta in &history {
        apply(&mut host, Scope(0), *delta);
    }
    let baseline = host.snapshot().unwrap();
    let outputs: Vec<_> = replay
        .iter()
        .map(|delta| apply(&mut host, Scope(0), *delta))
        .collect();
    let expected: BTreeMap<_, _> = host.inspect().states.clone();
    assert_eq!(host.restore(&baseline), Err(Error::Unavailable));
    host.pause();
    assert_eq!(
        host.replace(Ledger {
            factor: 2,
            version: 2
        }),
        Err(Error::InvalidState)
    );
    assert_eq!(host.inspect().states, &expected);
    host.restore(&baseline).unwrap();
    host.resume();
    let repeated: Vec<_> = replay
        .iter()
        .map(|delta| apply(&mut host, Scope(0), *delta))
        .collect();
    assert_eq!(
        repeated, outputs,
        "rejected replacement must retain the old code"
    );
    assert_eq!(host.inspect().states, &expected);
    host.pause();
    host.restore(&baseline).unwrap();
    host.replace(Ledger {
        factor: 2,
        version: 1,
    })
    .unwrap();
    host.resume();
    let mut expected_history = history;
    for delta in replay {
        expected_history.push(delta * 2);
        assert_eq!(
            apply(&mut host, Scope(0), delta),
            json!(expected_history.iter().sum::<i64>())
        );
    }
    assert_eq!(host.state(Scope(0)), Some(&state(&expected_history)));
    host.open(Scope(1)).unwrap();
    let before = host.inspect().states.clone();
    host.pause();
    assert_eq!(host.restore(&baseline), Err(Error::InvalidState));
    assert_eq!(host.inspect().states, &before);
}

#[hegel::test]
fn capacity_bounds_outstanding_work_and_pause_still_allows_drain(tc: TestCase) {
    let capacity = tc.draw(gs::integers::<usize>().max_value(16));
    let mut host = Executor::new(Ledger::default(), capacity).unwrap();
    let delta = tc.draw(gs::integers::<i64>());
    let tickets: Vec<_> = (0..capacity)
        .map(|_| host.submit(None, call(delta, 0, 0, false)).unwrap())
        .collect();
    assert_eq!(
        host.submit(None, call(delta, 0, 0, false)),
        Err(Error::Capacity)
    );
    host.pause();
    assert_eq!(
        host.submit(None, call(delta, 0, 0, false)),
        Err(Error::Unavailable)
    );
    for ticket in tickets {
        assert_eq!(host.step(), Some(Event::Accepted(ticket)));
        assert_eq!(
            host.step(),
            Some(Event::Completed {
                ticket,
                outcome: Ok(json!(delta))
            })
        );
    }
    assert_eq!(host.step(), None);
    assert!(host.idle());
    assert_eq!(
        host.submit(None, call(delta, 0, 0, false)),
        Err(Error::Unavailable)
    );
    host.resume();
    let result = host.submit(None, call(delta, 0, 0, false));
    if capacity == 0 {
        assert_eq!(result, Err(Error::Capacity));
    } else {
        assert!(result.is_ok(), "completion must release admission capacity");
    }
}
