//! Inbox tests cross the seam the production code relies on: a carrier thread
//! filling an inbox while the application thread empties it, so a missing
//! ordering guarantee shows up as a hang or a torn value here rather than as a
//! rare production stall.

use snap_transport::{
    Command, Error, Event, Invocation, Response, Value,
    carrier::{Connection, Frame, Submission},
    inbox::Inbox,
    json,
};
use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

fn invoke(id: u64) -> Command {
    Command::Invoke(Invocation {
        id,
        operation: "document.mutate".into(),
        input: Value::Null,
    })
}

#[test]
fn commands_cross_in_order() {
    let inbox = Inbox::new(64 * 1024);
    assert_eq!(
        inbox.submit(invoke(1), 16),
        Ok(Submission::Queued),
        "a queued command is a handoff, not acceptance"
    );
    assert_eq!(
        inbox.submit(invoke(2), 16),
        Ok(Submission::Queued),
        "queueing does not wait on the application"
    );
    assert_eq!(inbox.queued(), 2);
    assert!(matches!(
        inbox.next_command(),
        Some(Command::Invoke(Invocation { id: 1, .. }))
    ));
    assert!(matches!(
        inbox.next_command(),
        Some(Command::Invoke(Invocation { id: 2, .. }))
    ));
    assert!(
        inbox.next_command().is_none(),
        "drain ends at the queue edge"
    );
    assert_eq!(inbox.queued(), 0);
}

#[test]
fn byte_budget_is_a_window_not_a_total() {
    let inbox = Inbox::new(64);
    assert_eq!(inbox.submit(invoke(1), 40), Ok(Submission::Queued));
    assert_eq!(
        inbox.submit(invoke(2), 40),
        Err(Error::Capacity),
        "a producer that outruns the application is refused, not buffered"
    );
    // Draining releases exactly what was reserved, so the budget recovers.
    assert!(inbox.next_command().is_some());
    assert_eq!(
        inbox.submit(invoke(3), 40),
        Ok(Submission::Queued),
        "a drained inbox regains its budget"
    );
    assert_eq!(inbox.submit(invoke(4), 40), Err(Error::Capacity));
}

#[test]
fn observations_cross_in_order() {
    let inbox = Inbox::new(1024);
    assert!(
        inbox.receive().is_none(),
        "nothing is published before the application publishes it"
    );
    assert!(inbox.publish(
        Response::Event(Event::Accepted { id: 1 }),
        false,
        false,
        1,
    ));
    assert!(inbox.publish(
        Response::Event(Event::Completed {
            id: 1,
            outcome: Ok(json!("done")),
        }),
        false,
        true,
        1,
    ));
    assert!(matches!(
        inbox.receive(),
        Some(Frame { response: Response::Event(Event::Accepted { id: 1 }), .. })
    ));
    assert!(matches!(
        inbox.receive(),
        Some(Frame { response: Response::Event(Event::Completed { id: 1, .. }), terminal: true, .. })
    ));
    assert!(inbox.receive().is_none());
}

#[test]
fn handshake_carries_attachment_only_once() {
    let inbox = Inbox::new(1024);
    assert!(inbox.publish_attachment(
        Response::Attached { resumed: false },
        300_000,
        "boot".into(),
        1,
    ));
    let handshake = inbox.receive().expect("handshake frame");
    assert!(handshake.handshake, "the reply to Connect is a handshake");
    let attachment = handshake.attachment.expect("attachment");
    assert_eq!(attachment.retention_ms, 300_000);
    assert_eq!(attachment.lifetime, "boot");

    assert!(inbox.publish(Response::Detached, false, false, 1));
    let ordinary = inbox.receive().expect("ordinary frame");
    assert!(
        !ordinary.handshake,
        "only the reply to Connect is a handshake"
    );
    assert!(ordinary.attachment.is_none());
}

#[test]
fn physical_loss_preserves_logical_residency() {
    let inbox = Inbox::new(1024);
    assert_eq!(inbox.submit(invoke(1), 8), Ok(Submission::Queued));
    inbox.disconnect();

    assert!(inbox.is_detached(), "physical teardown is recorded");
    assert!(
        !inbox.retired(),
        "physical loss does not retire a logical connection"
    );
    assert!(
        inbox.next_command().is_some(),
        "queued work survives physical loss"
    );
}

#[test]
fn retirement_closes_submission_after_final_frames() {
    let inbox = Inbox::new(1024);
    assert!(inbox.publish(
        Response::Event(Event::Completed {
            id: 1,
            outcome: Ok(json!("done")),
        }),
        false,
        true,
        1
    ));
    inbox.retire();

    assert!(inbox.retired(), "the carrier observes retirement");
    assert!(
        inbox.receive().is_some(),
        "final frames must be drainable after retirement"
    );
    assert_eq!(
        inbox.submit(invoke(9), 8),
        Ok(Submission::CloseSocket),
        "a retired inbox asks for teardown instead of accepting more work"
    );
}

/// Depth and byte budget are independent limits. `serve` offers a 16 MiB budget,
/// and a connection must not reserve storage proportional to it.
#[test]
fn a_large_budget_does_not_reserve_proportional_depth() {
    let depth = Inbox::new(16 * 1024 * 1024).depth();
    assert!(
        (256..=1024).contains(&depth),
        "16 MiB budget must not buy {depth} eager slots, got {depth}"
    );
    // The byte budget is still fully enforced, so a large message is still
    // accepted and a second one is still refused.
    let inbox = Inbox::new(16 * 1024 * 1024);
    assert_eq!(inbox.submit(invoke(1), 4 * 1024 * 1024), Ok(Submission::Queued));
    assert_eq!(
        inbox.submit(invoke(2), 4 * 1024 * 1024),
        Ok(Submission::Queued),
        "the byte budget, not the slot count, bounds a large message"
    );
    assert_eq!(
        inbox.submit(invoke(3), 16 * 1024 * 1024),
        Err(Error::Capacity),
        "a message over budget is refused whatever the depth"
    );
}

/// A carrier task and an application task, joined, with enough traffic to catch
/// a missing release/acquire edge in either direction.
#[test]
fn producer_and_consumer_never_tear_a_value() {
    const COUNT: usize = 4096;
    let inbox = Arc::new(Inbox::new(8 * 1024));
    let barrier = Arc::new(Barrier::new(2));
    let observed = Arc::new(AtomicUsize::new(0));

    let producer = {
        let (inbox, barrier) = (inbox.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            for id in 1..=COUNT as u64 {
                // A refused submission is correct backpressure; retry so the
                // totals still line up.
                loop {
                    match inbox.submit(invoke(id), 8) {
                        Ok(Submission::Queued) => break,
                        Ok(Submission::CloseSocket) => panic!("inbox retired mid-run"),
                        Err(Error::Capacity) => thread::yield_now(),
                        Err(error) => panic!("unexpected {error:?}"),
                    }
                }
            }
        })
    };

    let consumer = {
        let (inbox, barrier, observed) = (inbox.clone(), barrier.clone(), observed.clone());
        thread::spawn(move || {
            barrier.wait();
            let mut highest = 0u64;
            while observed.load(Ordering::Relaxed) < COUNT {
                let Some(Command::Invoke(Invocation { id, .. })) = inbox.next_command() else {
                    thread::yield_now();
                    continue;
                };
                assert_eq!(id, highest + 1, "commands must not be reordered");
                highest = id;
                observed.fetch_add(1, Ordering::Relaxed);
            }
            highest
        })
    };

    producer.join().expect("producer");
    let highest = consumer.join().expect("consumer");
    assert_eq!(highest, COUNT as u64);
    assert_eq!(inbox.queued(), 0);
}

/// Acceptance, publication and teardown race against each other: a carrier
/// draining final frames while the application retires the inbox.
#[test]
fn retirement_races_a_draining_carrier_without_losing_frames() {
    const FRAMES: usize = 2048;
    // Enough reply capacity that backpressure is not the thing under test.
    let inbox = Arc::new(Inbox::new(1024 * 1024));
    let barrier = Arc::new(Barrier::new(2));

    let publisher = {
        let (inbox, barrier) = (inbox.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            for _ in 0..FRAMES {
                // The application owns this direction, so a full reply queue is
                // backpressure it applies rather than a frame it loses.
                while !inbox.publish(Response::Detached, false, false, 1) {
                    thread::yield_now();
                }
            }
            inbox.retire();
        })
    };

    let carrier = {
        let (inbox, barrier) = (inbox.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            let mut drained = 0;
            loop {
                if inbox.receive().is_some() {
                    drained += 1;
                    continue;
                }
                if inbox.retired() {
                    break;
                }
                thread::yield_now();
            }
            drained
        })
    };

    publisher.join().expect("publisher");
    let drained = carrier.join().expect("carrier");
    assert_eq!(
        drained, FRAMES,
        "retirement must not truncate frames the application already published"
    );
}

/// A failed exchange keeps its mutation outcome unknown, so nothing replays it.
#[test]
fn failure_is_terminal_for_the_inbox() {
    let inbox = Inbox::new(1024);
    assert!(inbox.publish(
        Response::Failed(Error::Unavailable),
        false,
        true,
        1
    ));
    inbox.retire();

    let Frame { response, .. } = inbox.receive().expect("failure frame");
    assert!(
        matches!(response, Response::Failed(Error::Unavailable)),
        "IO failure does not become a retryable outcome"
    );
    assert_eq!(inbox.submit(invoke(1), 8), Ok(Submission::CloseSocket));
    assert!(
        inbox.receive().is_none(),
        "a failure frame is terminal; nothing follows it on the inbox"
    );
}