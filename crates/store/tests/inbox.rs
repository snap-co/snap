//! Inbox tests cross the seam production code relies on: a carrier thread filling
//! a channel while the application thread drains it, so a missing ordering
//! guarantee surfaces here as a hang or a torn value rather than as a rare
//! production stall.

use snap_store::inbox::{Channel, Rejected};
use std::{
    sync::{Arc, Barrier},
    thread,
};

fn channel(budget: usize) -> Arc<Channel<u64>> {
    Arc::new(Channel::new(1024, budget))
}

#[test]
fn items_cross_in_order() {
    let channel = channel(1024);
    assert!(channel.push(1, 8).is_ok());
    assert!(channel.push(2, 8).is_ok());
    assert_eq!(channel.len(), 2);
    assert_eq!(channel.pop(), Some((1, 8)));
    assert_eq!(channel.pop(), Some((2, 8)));
    assert_eq!(channel.pop(), None, "drain ends at the queue edge");
    assert!(channel.is_empty());
}

#[test]
fn budget_is_a_window_not_a_total() {
    let channel = channel(64);
    assert!(channel.push(1, 40).is_ok());
    assert_eq!(
        channel.push(2, 40),
        Err((2, Rejected::Full)),
        "a producer that outruns the consumer is refused, not buffered"
    );
    // Draining releases exactly what was reserved.
    assert_eq!(channel.pop(), Some((1, 40)));
    assert!(
        channel.push(3, 40).is_ok(),
        "a drained channel regains its budget"
    );
    assert_eq!(channel.push(4, 40), Err((4, Rejected::Full)));
}

#[test]
fn maximum_budget_does_not_allow_charge_overflow() {
    let channel = Channel::new(4, usize::MAX);
    assert!(channel.push(1, usize::MAX).is_ok());
    assert_eq!(channel.push(2, 1), Err((2, Rejected::Full)));
    assert_eq!(channel.pop(), Some((1, usize::MAX)));
    assert!(channel.push(3, usize::MAX).is_ok());
}

#[test]
fn concurrent_callers_transfer_owned_values_and_restore_budget() {
    const COUNT: usize = 128;
    let channel = Arc::new(Channel::new(8, 7));
    let barrier = Arc::new(Barrier::new(4));
    thread::scope(|scope| {
        for producer in 0..2 {
            let (channel, barrier) = (channel.clone(), barrier.clone());
            scope.spawn(move || {
                barrier.wait();
                for index in 0..COUNT {
                    let mut value = format!("{producer}:{index}");
                    loop {
                        match channel.push(value, 1) {
                            Ok(()) => break,
                            Err((returned, Rejected::Full)) => value = returned,
                        }
                        thread::yield_now();
                    }
                }
            });
        }
        let consumers: Vec<_> = (0..2)
            .map(|_| {
                let (channel, barrier) = (channel.clone(), barrier.clone());
                scope.spawn(move || {
                    barrier.wait();
                    let mut received = Vec::new();
                    while received.len() < COUNT {
                        if let Some((value, bytes)) = channel.pop() {
                            assert_eq!(bytes, 1);
                            received.push(value);
                        } else {
                            thread::yield_now();
                        }
                    }
                    received
                })
            })
            .collect();
        let mut received: Vec<_> = consumers
            .into_iter()
            .flat_map(|consumer| consumer.join().unwrap())
            .collect();
        received.sort();
        let mut expected: Vec<_> = (0..2)
            .flat_map(|producer| (0..COUNT).map(move |index| format!("{producer}:{index}")))
            .collect();
        expected.sort();
        assert_eq!(received, expected);
    });
    assert!(channel.is_empty());
    assert!(channel.push(String::from("entire budget"), 7).is_ok());
    assert_eq!(
        channel.push(String::from("over budget"), 1),
        Err((String::from("over budget"), Rejected::Full))
    );
}

#[test]
fn slot_limit_bounds_a_zero_budget() {
    let channel = Arc::new(Channel::<u64>::new(4, usize::MAX));
    for value in 1..=4 {
        assert!(channel.push(value, 0).is_ok());
    }
    assert_eq!(
        channel.push(5, 0),
        Err((5, Rejected::Full)),
        "slot exhaustion refuses rather than overwriting an unread item"
    );
    assert_eq!(channel.pop(), Some((1, 0)), "oldest item is retained");
    assert!(channel.push(6, 0).is_ok(), "a freed slot is reusable");
}

#[test]
fn rejected_value_comes_back() {
    let channel = channel(8);
    assert!(channel.push(1, 8).is_ok());
    assert_eq!(
        channel.push(2, 8),
        Err((2, Rejected::Full)),
        "a refused item is returned so a caller never loses a decoded value"
    );
}

#[test]
fn capacity_rounds_up_to_a_power_of_two() {
    assert_eq!(Channel::<u64>::new(3, 0).capacity(), 4);
    assert_eq!(Channel::<u64>::new(16, 0).capacity(), 16);
    assert_eq!(Channel::<u64>::new(0, 0).capacity(), 1, "never zero");
}

/// Wrap-around correctness: the cursors are compared with wrapping arithmetic, so
/// a full lap must not read as empty or as full.
#[test]
fn cursors_survive_wrapping_many_times() {
    let channel = Arc::new(Channel::<u64>::new(8, usize::MAX));
    for lap in 0..1000u64 {
        for offset in 0..8 {
            assert!(channel.push(lap * 8 + offset, 1).is_ok());
        }
        for offset in 0..8 {
            assert_eq!(channel.pop(), Some((lap * 8 + offset, 1)));
        }
    }
    assert!(channel.is_empty());
}

/// A producer thread and a consumer thread, joined, with enough traffic that a
/// missing release/acquire edge on either cursor would reorder or duplicate.
#[test]
fn producer_and_consumer_never_tear_a_value() {
    const COUNT: u64 = 20_000;
    let channel = Arc::new(Channel::<u64>::new(64, 1024));
    let barrier = Arc::new(Barrier::new(2));

    let producer = {
        let (channel, barrier) = (channel.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            for value in 1..=COUNT {
                // A refusal is correct backpressure; retry so totals line up.
                while channel.push(value, 1).is_err() {
                    thread::yield_now();
                }
            }
        })
    };

    let consumer = {
        let (channel, barrier) = (channel.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            let mut expected = 1u64;
            while expected <= COUNT {
                match channel.pop() {
                    Some((value, _)) => {
                        assert_eq!(value, expected, "items must not be reordered");
                        expected += 1;
                    }
                    None => thread::yield_now(),
                }
            }
        })
    };

    producer.join().expect("producer");
    consumer.join().expect("consumer");
    assert!(channel.is_empty());
}

/// Both directions at once, the way a carrier and an application actually run.
#[test]
fn bidirectional_traffic_keeps_each_direction_separate() {
    const ROUNDS: u64 = 5_000;
    let requests = Arc::new(Channel::<u64>::new(64, 4096));
    let replies = Arc::new(Channel::<u64>::new(64, 4096));
    let barrier = Arc::new(Barrier::new(2));

    let carrier = {
        let (requests, replies, barrier) = (requests.clone(), replies.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            for id in 1..=ROUNDS {
                while requests.push(id, 1).is_err() {
                    thread::yield_now();
                }
            }
            for id in 1..=ROUNDS {
                while replies.push(id * 10, 1).is_err() {
                    thread::yield_now();
                }
            }
        })
    };

    let application = {
        let (requests, replies, barrier) = (requests.clone(), replies.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            let mut seen = 0u64;
            while seen < ROUNDS * 2 {
                if let Some((value, _)) = requests.pop() {
                    assert!(value <= ROUNDS, "requests stay in their own direction");
                    seen += 1;
                }
                if let Some((value, _)) = replies.pop() {
                    assert!(
                        value % 10 == 0 && value / 10 <= ROUNDS,
                        "replies stay in their own direction"
                    );
                    seen += 1;
                }
                if seen < ROUNDS * 2 {
                    thread::yield_now();
                }
            }
        })
    };

    carrier.join().expect("carrier");
    application.join().expect("application");
    assert!(requests.is_empty() && replies.is_empty());
}

/// Dropping with items still queued must run their destructors exactly once.
#[test]
fn drop_drains_queued_items_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let channel = Channel::new(16, 1024);
    for _ in 0..5 {
        assert!(channel.push(Counted(drops.clone()), 1).is_ok());
    }
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "queued items are still owned by the channel"
    );
    let popped = channel.pop().expect("one item drains");
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "a popped item is owned by the caller"
    );
    drop(popped);
    assert_eq!(drops.load(Ordering::SeqCst), 1, "exactly once");
    drop(channel);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        5,
        "the remaining four are dropped by the channel"
    );
}
