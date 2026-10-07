//! Client routing tests. Each case feeds frames one at a time, because that is
//! now the only shape the wire carries: batching was an artifact of the old
//! single-response exchange, not a protocol guarantee.

use snap_transport::{
    Channel, Command, Error, Event, Invocation, Response,
    client::{Client, Pump},
    json,
};
use std::{
    collections::VecDeque,
    future::Future,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

/// A channel whose replies are scripted. Records what was sent so tests can
/// assert the client queued a command before waiting on any frame.
struct Script {
    sent: Arc<Mutex<Vec<Command>>>,
    incoming: VecDeque<Response>,
}

impl Script {
    fn new(incoming: Vec<Response>) -> (Self, Arc<Mutex<Vec<Command>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sent: sent.clone(),
                incoming: incoming.into(),
            },
            sent,
        )
    }
}

impl Channel for Script {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        self.sent.lock().unwrap().push(command);
        Ok(())
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        Ok(self.incoming.pop_front())
    }
}

fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("unexpected IO"),
    }
}

fn accepted(id: u64) -> Response {
    Response::Event(Event::Accepted { id })
}
fn progress(id: u64, value: serde_json::Value) -> Response {
    Response::Event(Event::Progress { id, value })
}
fn completed(id: u64, outcome: snap_transport::Outcome) -> Response {
    Response::Event(Event::Completed { id, outcome })
}
fn global(kind: &str, input: serde_json::Value) -> Response {
    Response::Global {
        kind: kind.into(),
        input,
    }
}

fn ok() -> snap_transport::Outcome {
    Ok(json!(1))
}

/// One invocation's frames arrive across three separate reads. This is the whole
/// reason batching was removed: acceptance must be publishable before the handler
/// runs, so a slow operation cannot leave the client unable to tell working from
/// hung.
#[test]
fn an_invocation_completes_across_separate_frames() {
    let (script, sent) = Script::new(vec![
        accepted(1),
        progress(1, json!("calling model")),
        completed(1, ok()),
    ]);
    let mut client = Client::new(script);

    let id = ready(client.begin("some.thing", json!({}))).unwrap();
    assert_eq!(id, 1);
    assert_eq!(
        client.outstanding(),
        1,
        "the call is tracked before any frame"
    );
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "sending must not wait for a reply");
    match &sent[0] {
        Command::Invoke(Invocation {
            id,
            operation,
            input,
        }) => {
            assert_eq!(*id, 1);
            assert_eq!(operation, "some.thing");
            assert_eq!(*input, json!({}));
        }
        other => panic!("unexpected command {other:?}"),
    }
    drop(sent);

    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: 1 });
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Progress {
            id: 1,
            value: json!("calling model")
        }
    );
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Completed {
            id: 1,
            outcome: ok()
        }
    );
    assert_eq!(client.outstanding(), 0, "completion retires the trace");
}

/// Progress is only meaningful between acceptance and completion. A frame that
/// arrives out of order means this trace no longer describes the wire, so it is
/// abandoned rather than allowed to resume.
#[test]
fn a_broken_ordering_discards_the_trace() {
    let cases = [
        // completion with no acceptance and no error
        vec![completed(1, ok())],
        // progress before acceptance
        vec![progress(1, json!("early")), accepted(1), completed(1, ok())],
        // acceptance for someone else's invocation
        vec![accepted(2), completed(1, ok())],
    ];
    for frames in cases {
        let label = format!("{frames:?}");
        let count = frames.len();
        let (script, _) = Script::new(frames);
        let mut client = Client::new(script);
        ready(client.begin("some.thing", json!({}))).unwrap();
        let mut discarded = false;
        // Pump the whole script: a frame for another id is discarded without
        // touching this trace, so the interesting discard may come later.
        for _ in 0..count {
            if ready(client.pump()).unwrap() == Pump::Discarded(Some(1)) {
                discarded = true;
            }
        }
        assert!(discarded, "out-of-order frames must be discarded: {label}");
        assert_eq!(client.outstanding(), 0, "a broken trace is abandoned");
    }
}

/// A frame for an invocation this client never sent is dropped, not surfaced. A
/// peer controls what it puts on the wire, so it must not be able to steer the
/// client's own call table.
#[test]
fn a_frame_for_an_unknown_invocation_is_dropped() {
    let (script, _) = Script::new(vec![accepted(999), completed(999, ok())]);
    let mut client = Client::new(script);
    assert_eq!(ready(client.pump()).unwrap(), Pump::Discarded(None));
    assert_eq!(ready(client.pump()).unwrap(), Pump::Discarded(None));
}

/// Two invocations may be outstanding at once. Each frame routes to its own
/// channel regardless of the order they complete in.
#[test]
fn interleaved_invocations_route_by_id() {
    let (script, _) = Script::new(vec![
        accepted(1),
        accepted(2),
        progress(2, json!("second")),
        completed(2, Ok(json!("b"))),
        completed(1, Ok(json!("a"))),
    ]);
    let mut client = Client::new(script);
    let first = ready(client.begin("one", json!({}))).unwrap();
    let second = ready(client.begin("two", json!({}))).unwrap();
    assert_eq!((first, second), (1, 2));
    assert_eq!(client.outstanding(), 2);

    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: 1 });
    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: 2 });
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Progress {
            id: 2,
            value: json!("second")
        }
    );
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Completed {
            id: 2,
            outcome: Ok(json!("b"))
        }
    );
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Completed {
            id: 1,
            outcome: Ok(json!("a"))
        }
    );
    assert_eq!(client.outstanding(), 0);
}

/// A global push reaches its registered handler and never disturbs an invocation
/// running on the same connection.
#[test]
fn a_global_push_reaches_its_handler() {
    let (script, _) = Script::new(vec![
        accepted(1),
        global("document.replication", json!({"intent":{"id":7}})),
        completed(1, ok()),
    ]);
    let mut client = Client::new(script);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    client.on_global("document.replication", move |input| {
        recorder.lock().unwrap().push(input);
    });
    ready(client.begin("document.mutate", json!({}))).unwrap();

    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: 1 });
    assert_eq!(ready(client.pump()).unwrap(), Pump::Global);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[json!({"intent":{"id":7}})],
        "the payload reaches the handler uninspected"
    );
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Completed {
            id: 1,
            outcome: ok()
        }
    );
}

/// A peer can publish frames for topics nobody subscribed to. Reporting those
/// would let any server turn an unhandled topic into a client-side failure, or
/// into a log flood, so they are dropped.
#[test]
fn an_unhandled_global_is_dropped_without_failing_the_client() {
    let (script, _) = Script::new(vec![
        global("nobody.listening", json!("junk")),
        accepted(1),
        completed(1, ok()),
    ]);
    let mut client = Client::new(script);
    ready(client.begin("some.thing", json!({}))).unwrap();
    assert_eq!(ready(client.pump()).unwrap(), Pump::Discarded(None));
    // The client keeps working.
    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: 1 });
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Completed {
            id: 1,
            outcome: ok()
        }
    );
}

/// Refusal is uncorrelated: the invocation never began, so no id owns it and
/// outstanding work cannot be resolved by it.
#[test]
fn refusal_is_uncorrelated_and_abandons_outstanding_work() {
    let (script, _) = Script::new(vec![accepted(1), Response::Failed(Error::UnknownOperation)]);
    let mut client = Client::new(script);
    ready(client.begin("some.thing", json!({}))).unwrap();
    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: 1 });
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Refused(Error::UnknownOperation)
    );
    assert_eq!(client.outstanding(), 0);
}

/// A credential change is applied only if the operation then succeeds. A failed
/// operation must not leave a new bearer behind.
#[test]
fn a_bearer_change_survives_only_a_successful_completion() {
    use snap_transport::bearer::{Change, Token};

    let (script, _) = Script::new(vec![
        accepted(1),
        Response::Event(Event::Bearer {
            id: 1,
            change: Change::Set(Token::new("fresh".into())),
        }),
        completed(1, Err(Error::Unavailable)),
    ]);
    let mut client = Client::new(script);
    ready(client.begin("some.thing", json!({}))).unwrap();
    ready(client.pump()).unwrap();
    ready(client.pump()).unwrap();
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Discarded(Some(1)),
        "a failed operation must not install its credential"
    );
    assert_eq!(client.bearer(), None);

    let (script, _) = Script::new(vec![
        accepted(1),
        Response::Event(Event::Bearer {
            id: 1,
            change: Change::Set(Token::new("fresh".into())),
        }),
        completed(1, ok()),
    ]);
    let mut client = Client::new(script);
    ready(client.begin("some.thing", json!({}))).unwrap();
    ready(client.pump()).unwrap();
    ready(client.pump()).unwrap();
    ready(client.pump()).unwrap();
    assert_eq!(client.bearer(), Some("fresh"));
}

/// A frame that breaks this invocation's own ordering contract abandons its
/// trace, so nothing later can resolve it. Awaiting must report that the call
/// is lost immediately rather than keep pumping — otherwise a known-lost call
/// waits for a frame that can never come and only resolves when the socket dies,
/// mislabelling a definite protocol failure as an unknown outcome.
#[test]
fn a_lost_trace_reports_immediately_rather_than_waiting_for_the_channel() {
    // Completion with a successful outcome and no acceptance is a violation.
    let (script, _) = Script::new(vec![completed(1, ok()), accepted(1)]);
    let mut client = Client::new(script);
    let id = ready(client.begin("some.thing", json!({}))).unwrap();
    assert_eq!(
        ready(client.await_outcome(id, |_| {})).unwrap_err(),
        Error::Protocol
    );

    // A frame for someone else's invocation is dropped and never touches this
    // trace, so the call keeps waiting and then reports the unknown outcome.
    let (script, _) = Script::new(vec![accepted(2), accepted(1)]);
    let mut client = Client::new(script);
    let id = ready(client.begin("some.thing", json!({}))).unwrap();
    assert_eq!(
        ready(client.await_outcome(id, |_| {})).unwrap_err(),
        Error::Unavailable,
        "a well-formed but unfinished call is unknown, not rejected"
    );
}

/// Ids are never reused, so a late frame from an abandoned connection cannot be
/// mistaken for the answer to a newer invocation.
#[test]
fn ids_are_never_reused_across_a_channel_replacement() {
    let (script, _) = Script::new(vec![]);
    let mut client = Client::new(script);
    assert_eq!(ready(client.begin("one", json!({}))).unwrap(), 1);
    assert_eq!(ready(client.begin("two", json!({}))).unwrap(), 2);

    client.replace_channel(Script::new(vec![]).0);
    assert_eq!(ready(client.begin("three", json!({}))).unwrap(), 3);
    assert_eq!(
        client.outstanding(),
        3,
        "outstanding invocations are retained across a replacement"
    );
}

#[test]
fn explicit_abandonment_drops_only_its_trace_and_never_replays_the_call() {
    let (script, old_sent) = Script::new(vec![accepted(1)]);
    let mut client = Client::new(script);
    let lost = ready(client.begin("mutation", json!(7))).unwrap();
    ready(client.pump()).unwrap();
    let other = ready(client.begin("other", json!(null))).unwrap();
    assert!(client.abandon(lost));
    assert!(!client.abandon(lost));
    assert_eq!(
        client.outstanding(),
        1,
        "abandonment must not erase another trace"
    );
    let (replacement, new_sent) = Script::new(vec![
        accepted(lost),
        completed(lost, ok()),
        accepted(other),
        completed(other, ok()),
    ]);
    client.replace_channel(replacement);
    assert!(
        new_sent.lock().unwrap().is_empty(),
        "recovery must not resend anything"
    );
    assert_eq!(ready(client.pump()).unwrap(), Pump::Discarded(None));
    assert_eq!(ready(client.pump()).unwrap(), Pump::Discarded(None));
    assert_eq!(ready(client.pump()).unwrap(), Pump::Accepted { id: other });
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Completed {
            id: other,
            outcome: ok()
        }
    );
    assert_eq!(ready(client.begin("fresh", json!(null))).unwrap(), 3);
    assert_eq!(old_sent.lock().unwrap().len(), 2);
    assert_eq!(
        new_sent.lock().unwrap().len(),
        1,
        "only the new call is sent"
    );
}

/// A replacement attachment restarts each trace from unaccepted, because frames
/// already observed went to the old channel. Completion is deliberately retained:
/// it may already be committed server-side, and refusing its report would strand
/// a known outcome.
#[test]
fn a_replaced_channel_restarts_acceptance_but_keeps_completion_possible() {
    let (script, _) = Script::new(vec![accepted(1)]);
    let mut client = Client::new(script);
    let id = ready(client.begin("some.thing", json!({}))).unwrap();
    ready(client.pump()).unwrap();

    client.replace_channel(Script::new(vec![completed(id, ok())]).0);
    // Acceptance was already observed on the old channel, so re-sending it is
    // required before a completion can be accepted again.
    assert_eq!(
        ready(client.pump()).unwrap(),
        Pump::Discarded(Some(id)),
        "the completion cannot arrive before a fresh acceptance"
    );
}
