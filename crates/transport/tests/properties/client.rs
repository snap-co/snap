//! The oracle is an observation grammar, independent of Client's slice matching.
use hegel::{TestCase, generators as gs};
use snap_transport::{Channel, Command, Error, Event, Outcome, Response, client::Client, json};
use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    pin::pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("scripted channel must not wait for IO"),
    }
}

#[derive(Default)]
struct Script {
    /// Scripted frames, oldest first. A channel is a stream, so a scripted
    /// history is a sequence of single-frame responses rather than one batch.
    frames: VecDeque<Result<Response, Error>>,
    commands: Vec<Command>,
}

#[derive(Clone)]
struct Scripted(Rc<RefCell<Script>>);

impl Channel for Scripted {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        self.0.borrow_mut().commands.push(command);
        Ok(())
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        match self.0.borrow_mut().frames.pop_front() {
            Some(Ok(response)) => Ok(Some(response)),
            Some(Err(error)) => Err(error),
            // The script ran dry: the channel closed.
            None => Ok(None),
        }
    }
}

/// Judge a correlated event sequence.
///
/// `Err` means the sequence broke its ordering contract, so the client's trace
/// is abandoned and that invocation can never resolve. `Ok(None)` means the
/// sequence is well formed but unfinished: the stream simply ran dry, leaving
/// the outcome unknown. `Ok(Some(_))` is a resolved outcome. The last two must
/// stay distinct — an unfinished call is never a definite rejection.
fn grammar(events: &[Event], expected_id: u64) -> Result<Option<Outcome>, Error> {
    let mut accepted = false;
    let mut terminal = None;
    let mut bearer = false;
    for event in events {
        if terminal.is_some() {
            return Err(Error::Protocol);
        }
        match event {
            Event::Accepted { id } => {
                if *id != expected_id {
                    return Err(Error::Protocol);
                }
                accepted = true;
            }
            Event::Progress { id, .. } => {
                if *id != expected_id || !accepted {
                    return Err(Error::Protocol);
                }
            }
            Event::Bearer { id, .. } => {
                if *id != expected_id || !accepted || bearer {
                    return Err(Error::Protocol);
                }
                bearer = true;
            }
            Event::Completed { id, outcome } => {
                if *id != expected_id
                    || (!accepted && outcome.is_ok())
                    || (bearer && outcome.is_err())
                {
                    return Err(Error::Protocol);
                }
                terminal = Some(outcome.clone());
            }
        }
    }
    Ok(terminal)
}

fn frames(tc: &TestCase, id: u64) -> Vec<Response> {
    let outcome = if tc.draw(gs::booleans()) {
        Ok(json!(tc.draw(gs::integers::<i64>())))
    } else {
        Err(Error::Application(json!("declined")))
    };
    // Include valid messages explicitly, so random garbage cannot make the test
    // pass by accepting Protocol for almost every example.
    match tc.draw(gs::integers::<u8>().max_value(7)) {
        0 => vec![
            Response::Event(Event::Accepted { id }),
            Response::Event(Event::Completed { id, outcome }),
        ],
        1 => vec![Response::Event(Event::Completed {
            id,
            outcome: Err(Error::Unavailable),
        })],
        2 => vec![Response::Failed(Error::Capacity)],
        3 => vec![Response::Attached {
            resumed: tc.draw(gs::booleans()),
        }],
        4 => vec![Response::Detached],
        5 => vec![
            Response::Event(Event::Accepted { id }),
            Response::Event(Event::Progress {
                id,
                value: json!("working"),
            }),
            // A repeated acceptance is a duplicate frame, not new work.
            Response::Event(Event::Accepted { id }),
            Response::Event(Event::Completed { id, outcome }),
        ],
        _ => {
            let len = tc.draw(gs::integers::<usize>().max_value(6));
            let mut events = Vec::new();
            for _ in 0..len {
                let id =
                    [id, id - 1, id + 1, u64::MAX][tc.draw(gs::integers::<usize>().max_value(3))];
                events.push(match tc.draw(gs::integers::<u8>().max_value(2)) {
                    0 => Event::Accepted { id },
                    1 => Event::Progress {
                        id,
                        value: json!("progress"),
                    },
                    _ => Event::Completed {
                        id,
                        outcome: outcome.clone(),
                    },
                });
            }
            events.into_iter().map(Response::Event).collect()
        }
    }
}

/// The events in a scripted history addressed to `expected_id`.
///
/// A frame naming some other invocation is routed before it reaches any trace,
/// so it is noise here: the client drops it and its own trace is untouched.
/// Folding those in would make `grammar` reject sequences the client accepts.
///
/// The sequence also stops at the terminal frame. A completion retires the
/// invocation, so anything after it can no longer resolve anything and the
/// client never even reads it.
fn events(frames: &[Response], expected_id: u64) -> Vec<Event> {
    let mut seen = Vec::new();
    for frame in frames {
        let Response::Event(event) = frame else {
            continue;
        };
        match event {
            Event::Accepted { id }
            | Event::Progress { id, .. }
            | Event::Bearer { id, .. }
            | Event::Completed { id, .. }
                if *id == expected_id => {}
            _ => continue,
        }
        let terminal = matches!(event, Event::Completed { .. });
        seen.push(event.clone());
        if terminal {
            break;
        }
    }
    seen
}

#[hegel::test]
fn response_histories_obey_correlation_and_acceptance_grammar(tc: TestCase) {
    let script = Scripted(Rc::default());
    let mut client = Client::new(script.clone());
    let count = tc.draw(gs::integers::<u64>().min_value(1).max_value(40));
    for id in 1..=count {
        let reply = frames(&tc, id);
        let correlated = events(&reply, id);
        let expected = match reply.as_slice() {
            [Response::Failed(error)] => Err(error.clone()),
            [Response::Event(_), ..] => match grammar(&correlated, id) {
                // A broken ordering contract abandons the trace, so no later
                // frame can resolve this invocation.
                Err(error) => Err(error),
                // Well formed so far, but the stream ran dry. The outcome is
                // genuinely unknown, never a definite rejection.
                Ok(None) => Err(Error::Unavailable),
                Ok(Some(outcome)) => outcome,
            },
            // A handshake frame cannot answer an invocation; it is discarded
            // and the outcome stays unknown.
            _ => Err(Error::Unavailable),
        };
        tc.note(&format!("id={id} reply={reply:?}"));
        tc.event(match &expected {
            Ok(_) => "successful correlated response",
            Err(Error::Protocol) => "malformed response rejected",
            Err(_) => "explicit error propagated",
        });
        script.0.borrow_mut().frames = reply.into_iter().map(Ok).collect();
        let request = tc.draw(gs::booleans());
        let actual = if request {
            ready(client.request(Some("bearer"), "operation", json!(id)))
        } else {
            ready(client.invoke("operation", json!(id)))
        };
        assert_eq!(actual, expected);
        let state = script.0.borrow();
        assert_eq!(
            state.commands.len(),
            id as usize,
            "one exchange per command"
        );
        let invocation = match state.commands.last().unwrap() {
            Command::Request { bearer, invocation } if request => {
                assert_eq!(bearer.as_deref(), Some("bearer"));
                invocation
            }
            Command::Invoke(invocation) if !request => invocation,
            other => panic!("wrong command kind: {other:?}"),
        };
        assert_eq!(invocation.id, id);
        assert_eq!(invocation.operation, "operation");
        assert_eq!(invocation.input, json!(id));
    }
}

#[hegel::test]
fn unknown_io_outcomes_are_never_replayed_on_replacement(tc: TestCase) {
    let mut script = Scripted(Rc::default());
    let mut client = Client::new(script.clone());
    let failures = tc.draw(gs::vecs(gs::booleans()).min_size(1).max_size(80));
    for (index, fail) in failures.into_iter().enumerate() {
        let id = index as u64 + 1;
        script.0.borrow_mut().frames = if fail {
            vec![Err(Error::Unavailable)]
        } else {
            vec![
                Ok(Response::Event(Event::Accepted { id })),
                Ok(Response::Event(Event::Completed {
                    id,
                    outcome: Ok(json!(id)),
                })),
            ]
        }
        .into();
        assert_eq!(
            ready(client.invoke("mutate", json!(id))),
            if fail {
                Err(Error::Unavailable)
            } else {
                Ok(json!(id))
            }
        );
        assert_eq!(script.0.borrow().commands.len(), 1);
        let Command::Invoke(call) = script.0.borrow().commands[0].clone() else {
            panic!("expected invocation")
        };
        assert_eq!(call.id, id);
        // Replacing a channel must not itself send anything. A new explicit call
        // gets a new ID even after the old result became unknown.
        script = Scripted(Rc::default());
        client.replace_channel(script.clone());
        assert!(script.0.borrow().commands.is_empty());
    }
}

#[hegel::test]
fn lifecycle_commands_require_their_own_response_kind(tc: TestCase) {
    let script = Scripted(Rc::default());
    let mut client = Client::new(script.clone());
    let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(30));
    for _ in 0..rounds {
        let command = tc.draw(gs::integers::<u8>().max_value(2));
        let reply = frames(&tc, 1);
        let expected = match reply.as_slice() {
            // Nothing scripted: the channel closed without answering.
            [] => Err(Error::Unavailable),
            [Response::Failed(error)] => Err(error.clone()),
            [Response::Attached { resumed }] if command == 0 => Ok(*resumed),
            [Response::Detached] if command != 0 => Ok(false),
            _ => Err(Error::Protocol),
        };
        script.0.borrow_mut().frames = reply.into_iter().map(Ok).collect();
        let result = match command {
            0 => ready(client.connect("credential", "tab")),
            1 => ready(client.disconnect()).map(|()| false),
            _ => ready(client.close()).map(|()| false),
        };
        assert_eq!(result, expected);
        let sent = script.0.borrow_mut().commands.pop().unwrap();
        match (command, sent) {
            (0, Command::Connect { bearer, client_id }) => {
                assert_eq!(bearer, "credential");
                assert_eq!(client_id, "tab");
            }
            (1, Command::Disconnect) | (2, Command::Close) => {}
            other => panic!("wrong lifecycle command: {other:?}"),
        }
        assert!(script.0.borrow().commands.is_empty());
    }
}
