use snap_transport::{
    Command, Event, Invocation, Response,
    execution::Runtime,
    json,
    server::{Config, Server},
};
use testy_server::development::{Control, Development};

#[test]
fn tools_can_hold_inspect_revert_replace_and_replay_through_transport() {
    let mut host = Development::new(
        Runtime::new(
            Server::new(testy::TestAuthority, Config::default()),
            snap_transport::execution::Executor::new(testy::App::default(), 16).unwrap(),
        ),
        |_, _| Ok(json!(1000)),
        |name| match name {
            "double-add" => Some(testy::App::with_add(|a, b| {
                a.checked_add(b.checked_mul(2)?)
            })),
            _ => None,
        },
    );
    let peer = host.open().unwrap();
    host.send(
        peer,
        Command::Request {
            bearer: None,
            invocation: Invocation {
                id: 1,
                operation: "health.up".into(),
                input: json!(null),
            },
        },
        0,
    )
    .unwrap();
    assert!(matches!(
        &host.drain(peer).unwrap()[1],
        Response::Event(Event::Completed {
            id: 1,
            outcome: Ok(value),
        }) if value == &json!({"status":"OK"})
    ));
    host.send(
        peer,
        Command::Connect {
            bearer: testy::BEARER.into(),
            client_id: "agent".into(),
        },
        0,
    )
    .unwrap();
    host.drain(peer).unwrap();
    let invoke = |id, operation: &str, input| {
        Command::Invoke(Invocation {
            id,
            operation: operation.into(),
            input,
        })
    };
    host.send(peer, invoke(2, "calc.start", json!(null)), 0)
        .unwrap();
    host.drain(peer).unwrap();
    host.send(peer, invoke(3, "calc.add", json!(42)), 0)
        .unwrap();
    host.drain(peer).unwrap();
    host.control(Control::Snapshot, 0).unwrap();
    host.control(Control::Breakpoint { enabled: true }, 0)
        .unwrap();
    host.send(peer, invoke(4, "calc.add_checked", json!(10)), 0)
        .unwrap();
    assert_eq!(host.inspect()["manual"], true);
    assert_eq!(
        host.drain(peer).unwrap(),
        vec![Response::Event(Event::Accepted { id: 4 })]
    );
    assert!(host.control(Control::Restore, 0).is_err());
    assert!(
        host.control(
            Control::Replace {
                program: "double-add".into()
            },
            0
        )
        .is_err()
    );
    host.control(Control::Step, 0).unwrap();
    let held = host.inspect();
    assert_eq!(held["active"]["waiting"], testy::CEILING);
    assert_eq!(held["states"][0]["state"]["accumulator"], 42);
    let ticket = held["active"]["ticket"].as_u64().unwrap();
    assert!(
        host.control(
            Control::Supply {
                ticket: ticket + 1,
                key: testy::CEILING.into(),
                value: json!(1000)
            },
            0
        )
        .is_err()
    );
    host.control(
        Control::Supply {
            ticket,
            key: testy::CEILING.into(),
            value: json!(1000),
        },
        0,
    )
    .unwrap();
    host.control(Control::Step, 0).unwrap();
    assert_eq!(host.inspect()["states"][0]["state"]["accumulator"], 52);
    host.drain(peer).unwrap();
    host.control(Control::Restore, 0).unwrap();
    host.control(
        Control::Replace {
            program: "double-add".into(),
        },
        0,
    )
    .unwrap();
    host.send(peer, invoke(5, "calc.add", json!(10)), 0)
        .unwrap();
    host.control(Control::Step, 0).unwrap();
    host.control(Control::Step, 0).unwrap();
    assert_eq!(host.inspect()["states"][0]["state"]["accumulator"], 62);
    host.drain(peer).unwrap();
    host.send(peer, invoke(6, "calc.add", json!(1)), 0).unwrap();
    host.lost(peer, 1);
    host.control(Control::Step, 1).unwrap();
    host.control(Control::Step, 1).unwrap();
    assert_eq!(host.inspect()["states"][0]["state"]["accumulator"], 64);
    // Expiry changes inspection even when no client submits another operation.
    assert!(!host.tick(300_000));
    assert_eq!(host.inspect()["states"][0]["state"]["accumulator"], 64);
    assert!(host.tick(300_001));
    // No accepted operation remains to drain, so retirement removes idle state
    // immediately. Only an in-flight accepted operation needs a queued release.
    assert_eq!(host.inspect()["releases"], 0);
    assert_eq!(host.inspect()["states"], json!([]));
    assert!(!host.tick(300_002));
}
