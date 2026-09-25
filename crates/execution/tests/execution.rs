use snap_execution::*;
use std::{cell::Cell, rc::Rc};

struct App {
    operations: [Operation; 1],
    entries: Rc<Cell<usize>>,
    factor: i64,
    version: u64,
}
impl App {
    fn new(entries: Rc<Cell<usize>>, factor: i64, version: u64) -> Self {
        Self {
            operations: [Operation {
                key: "add",
                identity_required: true,
                input: |value| value["add"].as_i64().is_some(),
                output: |value| value.as_i64().is_some(),
                error: |value| value == "Rejected",
            }],
            entries,
            factor,
            version,
        }
    }
    fn handle(&self, call: &Call, work: &mut WorkingSet<'_>) -> Result<Value, Stop> {
        let before = work.state.as_i64().unwrap_or(0);
        work.state = json!(before + call.input["add"].as_i64().unwrap() * self.factor);
        match call.input["mode"].as_str() {
            Some("reads") => {
                work.inputs.read("first")?;
                work.inputs.read("second")?;
            }
            Some("error") => return Err(Error::Application(json!("Rejected")).into()),
            Some("undeclared") => return Err(Error::Application(json!("other")).into()),
            Some("bad-output") => return Ok(Value::Null),
            Some("bad-state") => {
                work.state = json!("invalid");
                return Ok(json!(0));
            }
            Some("repeat") => return Err(Stop::Need("first".into())),
            _ => {}
        }
        Ok(work.state.clone())
    }
}
impl Program for App {
    fn state_version(&self) -> u64 {
        self.version
    }
    fn valid_state(&self, state: &Value) -> bool {
        state.is_null() || state.as_i64().is_some()
    }
    fn operations(&self) -> &[Operation] {
        &self.operations
    }
    fn admit(&self, call: &Call, view: View<'_>) -> Admission {
        if call.input["mode"] == "guard" {
            return match view.inputs.read("guard") {
                Err(Stop::Need(key)) => Admission::Need(key),
                Ok(value) if value == true => Admission::Ready,
                _ => Admission::Reject(Error::Application(json!("Rejected"))),
            };
        }
        Admission::Ready
    }
    fn attempt(&self, call: &Call, mut work: WorkingSet<'_>) -> Attempt {
        self.entries.set(self.entries.get() + 1);
        let result = self.handle(call, &mut work);
        work.finish(result)
    }
}
fn call(add: i64, mode: &str) -> Call {
    Call {
        operation: "add".into(),
        identity: Some("verified".into()),
        input: json!({"add": add, "mode": mode}),
    }
}
fn executor() -> (Executor<App>, Rc<Cell<usize>>) {
    let entries = Rc::new(Cell::new(0));
    let mut host = Executor::new(App::new(entries.clone(), 1, 1), 8).unwrap();
    host.open(Scope(1)).unwrap();
    (host, entries)
}
fn run(host: &mut Executor<App>, amount: i64) -> Value {
    let ticket = host.submit(Some(Scope(1)), call(amount, "")).unwrap();
    assert_eq!(host.step(), Some(Event::Accepted(ticket)));
    match host.step().unwrap() {
        Event::Completed {
            ticket: done,
            outcome: Ok(value),
        } if done == ticket => value,
        event => panic!("unexpected {event:?}"),
    }
}

#[test]
fn whole_operation_gate_survives_multiple_misses_and_serializes_all_scopes() {
    let (mut host, entries) = executor();
    assert_eq!(run(&mut host, 42), json!(42));
    host.open(Scope(2)).unwrap();
    let first = host.submit(Some(Scope(1)), call(10, "reads")).unwrap();
    let second = host.submit(Some(Scope(1)), call(20, "")).unwrap();
    let other_scope = host.submit(Some(Scope(2)), call(9, "")).unwrap();
    assert_eq!(host.step(), Some(Event::Accepted(first)));
    assert_eq!(entries.get(), 1, "ACK precedes handler entry");
    assert_eq!(
        host.step(),
        Some(Event::Need {
            ticket: first,
            key: "first".into()
        })
    );
    assert_eq!(
        host.state(Scope(1)),
        Some(&json!(42)),
        "private write rolled back"
    );
    assert_eq!(
        host.step(),
        None,
        "no dispatch while active operation waits"
    );
    assert_eq!(host.state(Scope(2)), Some(&Value::Null));
    assert_eq!(
        host.supply(second, "first", Ok(json!(1))),
        Err(Error::Protocol)
    );
    host.supply(first, "first", Ok(json!(1))).unwrap();
    assert_eq!(
        host.step(),
        Some(Event::Need {
            ticket: first,
            key: "second".into()
        })
    );
    assert_eq!(host.state(Scope(1)), Some(&json!(42)));
    host.supply(first, "second", Ok(json!(2))).unwrap();
    assert_eq!(
        host.step(),
        Some(Event::Completed {
            ticket: first,
            outcome: Ok(json!(52))
        })
    );
    assert_eq!(host.step(), Some(Event::Accepted(second)));
    assert_eq!(
        host.step(),
        Some(Event::Completed {
            ticket: second,
            outcome: Ok(json!(72))
        })
    );
    assert_eq!(host.step(), Some(Event::Accepted(other_scope)));
    assert_eq!(
        host.step(),
        Some(Event::Completed {
            ticket: other_scope,
            outcome: Ok(json!(9))
        })
    );
    assert!(host.idle());
}

#[test]
fn admission_reads_finish_before_ack_and_rejection_never_enters_handler() {
    let (mut host, entries) = executor();
    for allowed in [false, true] {
        let ticket = host.submit(Some(Scope(1)), call(1, "guard")).unwrap();
        assert_eq!(
            host.step(),
            Some(Event::Need {
                ticket,
                key: "guard".into()
            })
        );
        assert_eq!(entries.get(), 0);
        host.supply(ticket, "guard", Ok(json!(allowed))).unwrap();
        if allowed {
            assert_eq!(host.step(), Some(Event::Accepted(ticket)));
            assert_eq!(entries.get(), 0);
            assert_eq!(
                host.step(),
                Some(Event::Completed {
                    ticket,
                    outcome: Ok(json!(1))
                })
            );
        } else {
            assert_eq!(
                host.step(),
                Some(Event::Completed {
                    ticket,
                    outcome: Err(Error::Application(json!("Rejected")))
                })
            );
        }
    }
}

#[test]
fn invalid_commits_and_application_errors_discard_tentative_edits() {
    let (mut host, _) = executor();
    run(&mut host, 42);
    for (mode, error) in [
        ("bad-output", Error::InvalidOutput),
        ("bad-state", Error::InvalidState),
        ("undeclared", Error::InvalidOutput),
        ("error", Error::Application(json!("Rejected"))),
    ] {
        let ticket = host.submit(Some(Scope(1)), call(10, mode)).unwrap();
        assert_eq!(host.step(), Some(Event::Accepted(ticket)));
        assert_eq!(
            host.step(),
            Some(Event::Completed {
                ticket,
                outcome: Err(error)
            })
        );
        assert_eq!(host.state(Scope(1)), Some(&json!(42)));
    }
}

#[test]
fn identity_and_schema_checks_precede_application_entry() {
    let (mut host, entries) = executor();
    let mut anonymous = call(1, "");
    anonymous.identity = None;
    let mut invalid = call(1, "");
    invalid.input = Value::Null;
    for (call, error) in [
        (anonymous, Error::IdentityRequired),
        (invalid, Error::InvalidInput),
    ] {
        let ticket = host.submit(Some(Scope(1)), call).unwrap();
        assert_eq!(
            host.step(),
            Some(Event::Completed {
                ticket,
                outcome: Err(error)
            })
        );
    }
    assert_eq!(entries.get(), 0);
}

#[test]
fn failed_dependency_and_repeated_need_release_gate_without_publication() {
    let (mut host, _) = executor();
    let ticket = host.submit(Some(Scope(1)), call(10, "reads")).unwrap();
    assert_eq!(host.step(), Some(Event::Accepted(ticket)));
    host.step().unwrap();
    host.supply(ticket, "first", Err(Error::Unavailable))
        .unwrap();
    assert_eq!(
        host.step(),
        Some(Event::Completed {
            ticket,
            outcome: Err(Error::Unavailable)
        })
    );
    assert_eq!(host.state(Scope(1)), Some(&Value::Null));
    let repeated = host.submit(Some(Scope(1)), call(10, "repeat")).unwrap();
    host.step();
    host.step();
    host.supply(repeated, "first", Ok(json!(1))).unwrap();
    assert_eq!(
        host.step(),
        Some(Event::Completed {
            ticket: repeated,
            outcome: Err(Error::Protocol)
        })
    );
    assert_eq!(run(&mut host, 1), json!(1));
}

#[test]
fn lifecycle_release_waits_for_owned_work_and_blocks_new_scope_submissions() {
    let (mut host, _) = executor();
    let ticket = host.submit(Some(Scope(1)), call(10, "reads")).unwrap();
    host.step();
    host.step();
    host.release(Scope(1));
    assert_eq!(
        host.submit(Some(Scope(1)), call(20, "")),
        Err(Error::Protocol)
    );
    assert_eq!(host.step(), None);
    assert!(host.state(Scope(1)).is_some());
    host.supply(ticket, "first", Err(Error::Unavailable))
        .unwrap();
    assert!(matches!(host.step(), Some(Event::Completed { .. })));
    assert_eq!(host.step(), None);
    assert!(host.state(Scope(1)).is_none());
    host.open(Scope(1)).unwrap();
    assert_eq!(
        host.supply(ticket, "first", Ok(json!(1))),
        Err(Error::Protocol)
    );
}

#[test]
fn code_replacement_drains_work_preserves_data_and_allows_snapshot_replay() {
    let (mut host, entries) = executor();
    run(&mut host, 42);
    let saved = host.snapshot().unwrap();
    let ticket = host.submit(Some(Scope(1)), call(10, "reads")).unwrap();
    host.step();
    host.step();
    host.pause();
    assert_eq!(
        host.submit(Some(Scope(1)), call(1, "")),
        Err(Error::Unavailable)
    );
    assert_eq!(
        host.replace(App::new(entries.clone(), 2, 1)),
        Err(Error::Unavailable)
    );
    assert!(matches!(host.snapshot(), Err(Error::Unavailable)));
    assert_eq!(host.restore(&saved), Err(Error::Unavailable));
    host.supply(ticket, "first", Ok(json!(1))).unwrap();
    host.step();
    host.supply(ticket, "second", Ok(json!(2))).unwrap();
    assert_eq!(
        host.step(),
        Some(Event::Completed {
            ticket,
            outcome: Ok(json!(52))
        })
    );
    assert_eq!(
        host.replace(App::new(entries.clone(), 2, 2)),
        Err(Error::InvalidState)
    );
    host.replace(App::new(entries, 2, 1)).unwrap();
    assert_eq!(host.state(Scope(1)), Some(&json!(52)));
    host.restore(&saved).unwrap();
    host.resume();
    assert_eq!(
        run(&mut host, 10),
        json!(62),
        "replacement code sees restored 42"
    );
    host.open(Scope(2)).unwrap();
    host.pause();
    assert_eq!(
        host.restore(&saved),
        Err(Error::InvalidState),
        "cannot resurrect a different scope graph"
    );
}

#[test]
fn request_local_state_never_becomes_resident() {
    let (mut host, _) = executor();
    for _ in 0..2 {
        let ticket = host.submit(None, call(10, "")).unwrap();
        assert_eq!(host.step(), Some(Event::Accepted(ticket)));
        assert_eq!(
            host.step(),
            Some(Event::Completed {
                ticket,
                outcome: Ok(json!(10))
            })
        );
    }
    assert_eq!(host.state(Scope(1)), Some(&Value::Null));
}
