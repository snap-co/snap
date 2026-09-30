//! Run with: cargo run -p testy-local --bin testy-execution-demo
use snap_transport::execution::{Call, Event, Executor, Scope, Value, json};

fn call(operation: &str, input: Value) -> Call {
    Call {
        operation: operation.into(),
        input,
        identity: Some(testy::IDENTITY.into()),
    }
}
fn finish(host: &mut Executor<testy::App>) {
    while let Some(event) = host.step() {
        match event {
            Event::Completed {
                outcome: Err(error),
                ..
            } => panic!("{error:?}"),
            Event::Need { .. } => panic!("unexpected dependency"),
            _ => {}
        }
    }
}
fn main() {
    let mut host = Executor::new(testy::App::default(), 16).unwrap();
    let calculator = Scope(1);
    host.open(calculator).unwrap();
    host.submit(Some(calculator), call("calc.start", Value::Null))
        .unwrap();
    finish(&mut host);
    host.submit(Some(calculator), call("calc.add", json!(42)))
        .unwrap();
    finish(&mut host);
    let saved = host.snapshot().unwrap();
    let first = host
        .submit(Some(calculator), call("calc.add_checked", json!(10)))
        .unwrap();
    let second = host
        .submit(Some(calculator), call("calc.add", json!(20)))
        .unwrap();
    assert_eq!(host.step(), Some(Event::Accepted(first)));
    let Some(Event::Need { ticket, key }) = host.step() else {
        panic!("expected missing ceiling");
    };
    assert_eq!(host.state(calculator).unwrap()["accumulator"], 42);
    assert_eq!(host.step(), None);
    println!("Missing ceiling after private +10: live value is 42; +20 remains queued");
    host.supply(ticket, &key, Ok(json!(100))).unwrap();
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
    println!("Supply ceiling: commit 52, then queued operation commits 72");
    host.pause();
    host.replace(testy::App::with_add(|a, b| {
        a.checked_add(b.checked_mul(2)?)
    }))
    .unwrap();
    assert_eq!(host.state(calculator).unwrap()["accumulator"], 72);
    host.restore(&saved).unwrap();
    host.resume();
    host.submit(Some(calculator), call("calc.add", json!(10)))
        .unwrap();
    finish(&mut host);
    assert_eq!(host.state(calculator).unwrap()["accumulator"], 62);
    println!("Replace add code, restore 42, replay +10: replacement commits 62");
}
