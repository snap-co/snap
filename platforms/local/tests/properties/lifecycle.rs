#[path = "../../../../crates/transport/tests/properties/execution_fixture.rs"]
mod fixture;

use fixture::{Ledger, state};
use hegel::{TestCase, generators as gs};
use snap_platform_local::{Observation, Peer, Platform, Submission};
use snap_transport::execution::{Executor, Scope, Ticket};
use snap_transport::{
    Command, Error, Event, Invocation, Response, json,
    server::{Config, Server},
};

type Host = Platform<Ledger, fn(&str) -> Option<String>>;

fn ready(host: &mut Host, peer: &mut Peer, command: Command, now: u64) -> Response {
    match host.submit(peer, command, now) {
        Submission::Ready(response) => response,
        Submission::Pending(_) => panic!("lifecycle command unexpectedly queued execution"),
    }
}

fn connect(index: usize) -> Command {
    Command::Connect {
        bearer: "valid".into(),
        client_id: format!("tab-{index}"),
    }
}

fn invoke(id: u64, delta: i64) -> Command {
    let call = fixture::call(delta, 1, 0, false);
    Command::Invoke(Invocation {
        id,
        operation: call.operation,
        input: call.input,
    })
}

fn event(host: &mut Host, ticket: Ticket, expected: Event) {
    match host.step() {
        Some(Observation::Event {
            ticket: actual_ticket,
            event,
            ..
        }) => {
            assert_eq!(actual_ticket, ticket);
            assert_eq!(event, expected);
        }
        _ => panic!("expected transport observation {expected:?}"),
    }
}

#[hegel::test]
fn retirement_revokes_dispatch_while_owned_work_drains(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().min_value(2).max_value(6));
    let retention = tc.draw(gs::integers::<u64>().max_value(10));
    let now = if tc.draw(gs::booleans()) {
        retention
    } else {
        retention.saturating_sub(1)
    };
    let mut host = Platform::new(
        Server::new(
            (|token: &str| (token == "valid").then(|| "alice".into()))
                as fn(&str) -> Option<String>,
            Config {
                reconnect_ms: retention,
                capacity: count,
            },
        ),
        Executor::new(Ledger::default(), count * 2).unwrap(),
    );
    let mut peers: Vec<_> = (0..count).map(|_| Peer::default()).collect();
    let mut jobs = Vec::new();
    let mut records = Vec::new();
    let mut retired = Vec::new();
    let mut detached = Vec::new();
    for (index, peer) in peers.iter_mut().enumerate() {
        let before = host.inspect().states.clone();
        assert_eq!(
            ready(&mut host, peer, connect(index), 0),
            Response::Attached { resumed: false }
        );
        let added: Vec<_> = host
            .inspect()
            .states
            .keys()
            .filter(|scope| !before.contains_key(scope))
            .copied()
            .collect();
        assert_eq!(added.len(), 1);
        let scope = added[0];
        records.push(scope);
        let delta = tc.draw(gs::integers::<i64>().min_value(-100).max_value(100));
        let Submission::Pending(ticket) = host.submit(peer, invoke(1, delta), 0) else {
            panic!("valid invocation must queue")
        };
        jobs.push((ticket, scope, delta));
    }
    event(&mut host, jobs[0].0, Event::Accepted { id: 1 });
    assert!(
        matches!(host.step(), Some(Observation::Need { ticket, ref key }) if ticket == jobs[0].0 && key == "input-0")
    );
    for (index, peer) in peers.iter_mut().enumerate() {
        let action = tc.draw(gs::integers::<u8>().max_value(3));
        tc.note(&format!(
            "peer={index} action={action} now={now} retention={retention}"
        ));
        match action {
            0 => {}
            1 => assert_eq!(
                ready(&mut host, peer, Command::Close, 0),
                Response::Detached
            ),
            2 => host.lost(peer, 0),
            _ => assert_eq!(
                ready(&mut host, peer, Command::Disconnect, 0),
                Response::Detached
            ),
        }
        retired.push(action == 1 || (action >= 2 && now >= retention));
        detached.push(action != 0);
        if action != 0 {
            match ready(&mut host, peer, invoke(2, 1), 0) {
                Response::Events(events) => assert_eq!(
                    events,
                    vec![Event::Completed {
                        id: 2,
                        outcome: Err(Error::IdentityRequired)
                    }]
                ),
                other => panic!("detached peer dispatched: {other:?}"),
            }
        }
    }
    host.tick(now);
    for (index, (ticket, _, _)) in jobs.iter().enumerate().skip(1) {
        if retired[index] {
            event(
                &mut host,
                *ticket,
                Event::Completed {
                    id: 1,
                    outcome: Err(Error::IdentityRequired),
                },
            );
        }
    }
    assert!(host.step().is_none(), "accepted work remains held");
    for (index, scope) in records.iter().enumerate() {
        assert_eq!(
            host.inspect().states.get(scope),
            (index == 0 || !retired[index]).then_some(&state(&[]))
        );
    }
    // Reattach while old work is still held. Expired/closed connections get fresh
    // scopes; retained connections keep the old scope and its queued operation.
    let mut reconnected = Vec::new();
    for (index, peer) in peers.iter_mut().enumerate() {
        let before = host.inspect().states.clone();
        let response = ready(&mut host, peer, connect(index), now);
        if index == 0 && retired[index] {
            assert_eq!(response, Response::Failed(Error::Occupied));
        } else if retired[index] {
            tc.event("retired scope replaced while work held");
            assert_eq!(response, Response::Attached { resumed: false });
            let added: Vec<_> = host
                .inspect()
                .states
                .keys()
                .filter(|scope| !before.contains_key(scope))
                .copied()
                .collect();
            assert_eq!(added.len(), 1);
            let new_scope = added[0];
            assert!(!records.contains(&new_scope));
            reconnected.push(new_scope);
        } else {
            tc.event("live or retained scope survives held work");
            assert_eq!(
                response,
                if detached[index] {
                    Response::Attached { resumed: true }
                } else {
                    Response::Failed(Error::Occupied)
                }
            );
            assert_eq!(host.inspect().states, &before);
        }
    }
    for (index, (ticket, scope, delta)) in jobs.iter().enumerate() {
        if index > 0 && retired[index] {
            continue;
        }
        if index > 0 {
            event(&mut host, *ticket, Event::Accepted { id: 1 });
            assert!(
                matches!(host.step(), Some(Observation::Need { ticket: actual, ref key }) if actual == *ticket && key == "input-0")
            );
        }
        assert_eq!(
            host.pending_call(*ticket).unwrap().identity.as_deref(),
            Some("alice")
        );
        let fail = tc.draw(gs::booleans());
        host.supply(
            *ticket,
            "input-0",
            if fail {
                Err(snap_transport::execution::Error::Unavailable)
            } else {
                Ok(json!(9))
            },
        )
        .unwrap();
        event(
            &mut host,
            *ticket,
            Event::Completed {
                id: 1,
                outcome: if fail {
                    Err(Error::Unavailable)
                } else {
                    Ok(json!(delta))
                },
            },
        );
        let expected = if fail { state(&[]) } else { state(&[*delta]) };
        assert_eq!(
            host.inspect().states.get(scope),
            (!retired[index]).then_some(&expected)
        );
    }
    assert!(host.step().is_none());
    for (scope, retired) in records.into_iter().zip(retired) {
        assert_eq!(host.inspect().states.contains_key(&scope), !retired);
    }
    for scope in reconnected {
        assert_eq!(
            host.inspect().states[&scope],
            state(&[]),
            "new attachment cannot inherit a retired scope's commit"
        );
    }
    assert!(host.inspect().active.is_none());
    assert!(host.inspect().queued.is_empty());
    assert_eq!(host.inspect().releases, 0);
}

#[hegel::test]
fn request_local_calls_never_create_connection_scopes(tc: TestCase) {
    let mut host = Platform::new(
        Server::new(
            (|_: &str| Some("alice".into())) as fn(&str) -> Option<String>,
            Config {
                reconnect_ms: 0,
                capacity: 1,
            },
        ),
        Executor::new(Ledger::default(), 1).unwrap(),
    );
    let mut peer = Peer::default();
    let values = tc.draw(
        gs::vecs(gs::integers::<i64>().min_value(-100).max_value(100))
            .min_size(1)
            .max_size(30),
    );
    for (id, delta) in values.into_iter().enumerate() {
        let call = fixture::call(delta, 0, 0, false);
        let Submission::Pending(ticket) = host.submit(
            &mut peer,
            Command::Request {
                bearer: Some("valid".into()),
                invocation: Invocation {
                    id: id as u64,
                    operation: call.operation,
                    input: call.input,
                },
            },
            0,
        ) else {
            panic!("request must queue")
        };
        event(&mut host, ticket, Event::Accepted { id: id as u64 });
        event(
            &mut host,
            ticket,
            Event::Completed {
                id: id as u64,
                outcome: Ok(json!(delta)),
            },
        );
        assert!(host.inspect().states.is_empty());
        assert!(host.inspect().active.is_none());
    }
    assert!(!host.inspect().states.contains_key(&Scope(0)));
}
