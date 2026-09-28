//! The oracle is an observation grammar, independent of Client's slice matching.
use hegel::{TestCase, generators as gs};
use snap_transport::{Channel, Command, Error, Event, Outcome, Response, client::Client, json};
use std::{
    cell::RefCell,
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
    response: Option<Result<Response, Error>>,
    commands: Vec<Command>,
}

#[derive(Clone)]
struct Scripted(Rc<RefCell<Script>>);

impl Channel for Scripted {
    async fn exchange(&mut self, command: Command) -> Result<Response, Error> {
        let mut script = self.0.borrow_mut();
        script.commands.push(command);
        script
            .response
            .take()
            .expect("client attempted an unsolicited exchange or replay")
    }
}

fn grammar(events: &[Event], expected_id: u64) -> Outcome {
    let mut accepted = false;
    let mut terminal = None;
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
            Event::Completed { id, outcome } => {
                if *id != expected_id || (!accepted && outcome.is_ok()) {
                    return Err(Error::Protocol);
                }
                terminal = Some(outcome.clone());
            }
        }
    }
    terminal.unwrap_or(Err(Error::Protocol))
}

fn response(tc: &TestCase, id: u64) -> Response {
    let outcome = if tc.draw(gs::booleans()) {
        Ok(json!(tc.draw(gs::integers::<i64>())))
    } else {
        Err(Error::Application(json!("declined")))
    };
    // Include valid messages explicitly, so random garbage cannot make the test
    // pass by accepting Protocol for almost every example.
    match tc.draw(gs::integers::<u8>().max_value(7)) {
        0 => Response::Events(vec![
            Event::Accepted { id },
            Event::Completed { id, outcome },
        ]),
        1 => Response::Events(vec![Event::Completed {
            id,
            outcome: Err(Error::Unavailable),
        }]),
        2 => Response::Failed(Error::Capacity),
        3 => Response::Attached {
            resumed: tc.draw(gs::booleans()),
        },
        4 => Response::Detached,
        5 => Response::Events(vec![
            Event::Accepted { id },
            Event::Progress {
                id,
                value: json!("working"),
            },
            Event::Accepted { id },
            Event::Completed { id, outcome },
        ]),
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
            Response::Events(events)
        }
    }
}

#[hegel::test]
fn response_histories_obey_correlation_and_acceptance_grammar(tc: TestCase) {
    let script = Scripted(Rc::default());
    let mut client = Client::new(script.clone());
    let count = tc.draw(gs::integers::<u64>().min_value(1).max_value(40));
    for id in 1..=count {
        let reply = response(&tc, id);
        let expected = match &reply {
            Response::Events(events) => grammar(events, id),
            Response::Failed(error) => Err(error.clone()),
            _ => Err(Error::Protocol),
        };
        tc.note(&format!("id={id} reply={reply:?}"));
        tc.event(match &expected {
            Ok(_) => "successful correlated response",
            Err(Error::Protocol) => "malformed response rejected",
            Err(_) => "explicit error propagated",
        });
        script.0.borrow_mut().response = Some(Ok(reply));
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
        script.0.borrow_mut().response = Some(if fail {
            Err(Error::Unavailable)
        } else {
            Ok(Response::Events(vec![
                Event::Accepted { id },
                Event::Completed {
                    id,
                    outcome: Ok(json!(id)),
                },
            ]))
        });
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
        let reply = response(&tc, 1);
        let expected = match &reply {
            Response::Failed(error) => Err(error.clone()),
            Response::Attached { resumed } if command == 0 => Ok(*resumed),
            Response::Detached if command != 0 => Ok(false),
            _ => Err(Error::Protocol),
        };
        script.0.borrow_mut().response = Some(Ok(reply));
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
