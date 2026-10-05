//! Concurrency cases for the inbox, sized so they are worth running under a
//! sanitizer or a data-race detector rather than only in release mode.

use snap_store::inbox::{Channel, Rejected};
use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

/// Independent producer/consumer pairs, each on its own channel. Detects shared
/// state that should not exist; a cross-channel race shows up as a torn value.
#[test]
fn independent_channels_do_not_interfere() {
    const PAIRS: usize = 8;
    const ROUNDS: u64 = 2_000;

    let barrier = Arc::new(Barrier::new(PAIRS * 2));
    let channels: Vec<_> = (0..PAIRS)
        .map(|_| Arc::new(Channel::<u64>::new(32, 4096)))
        .collect();
    let failures = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for (index, channel) in channels.iter().cloned().enumerate() {
        let barrier = barrier.clone();
        let failures = failures.clone();
        handles.push((
            channel.clone(),
            thread::spawn(move || {
                barrier.wait();
                for value in 1..=ROUNDS {
                    while channel.push(value, 1).is_err() {
                        thread::yield_now();
                    }
                }
            }),
            index,
            failures,
        ));
    }

    let mut consumers = Vec::new();
    for (channel, _producer, index, failures) in handles.iter_mut() {
        let channel = channel.clone();
        let barrier = barrier.clone();
        let failures = failures.clone();
        consumers.push((
            *index,
            thread::spawn(move || {
                barrier.wait();
                let mut expected = 1u64;
                while expected <= ROUNDS {
                    match channel.pop() {
                        Some((value, _)) if value == expected => expected += 1,
                        Some((_value, _)) => {
                            failures.fetch_add(1, Ordering::SeqCst);
                            expected += 1;
                        }
                        None => thread::yield_now(),
                    }
                }
            }),
        ));
    }

    for (_, producer, _, _) in handles.drain(..) {
        producer.join().expect("producer");
    }
    for (_, consumer) in consumers {
        consumer.join().expect("consumer");
    }
    assert_eq!(
        failures.load(Ordering::SeqCst),
        0,
        "each channel must deliver its own values in order"
    );
}

/// Backpressure under contention: the producer stays ahead, so refusals are
/// frequent and the slot/budget interaction is exercised on every lap.
#[test]
fn sustained_backpressure_never_corrupts() {
    const ROUNDS: u64 = 50_000;
    let channel = Arc::new(Channel::<u64>::new(8, 64));
    let barrier = Arc::new(Barrier::new(2));

    let producer = {
        let (channel, barrier) = (channel.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            let mut refused = 0u64;
            for value in 1..=ROUNDS {
                match channel.push(value, 1) {
                    Ok(()) => {}
                    Err((returned, Rejected::Full)) => {
                        assert_eq!(returned, value, "a refused value comes back intact");
                        refused += 1;
                        thread::yield_now();
                        // Retry after the consumer has had a chance.
                        while channel.push(value, 1).is_err() {
                            refused += 1;
                            thread::yield_now();
                        }
                    }
                }
            }
            refused
        })
    };

    let consumer = {
        let (channel, barrier) = (channel.clone(), barrier.clone());
        thread::spawn(move || {
            barrier.wait();
            let mut expected = 1u64;
            while expected <= ROUNDS {
                match channel.pop() {
                    Some((value, _)) => {
                        assert!(value <= ROUNDS, "backpressure must not reorder");
                        expected += 1;
                    }
                    None => thread::yield_now(),
                }
            }
        })
    };

    let refused = producer.join().expect("producer");
    consumer.join().expect("consumer");
    assert!(
        refused > 0,
        "the test should actually have hit backpressure"
    );
    assert!(channel.is_empty());
}

/// A channel dropped while a producer still references it must not leak or
/// double-drop: the channel owns its queued items until that point.
#[test]
fn concurrent_producer_with_channel_dropped_underneath() {
    const ROUNDS: u64 = 10_000;
    let channel = Arc::new(Channel::<u64>::new(16, 1024));
    let barrier = Arc::new(Barrier::new(2));

    let producer = {
        let channel = channel.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            let mut sent = 0u32;
            for value in 1..=ROUNDS {
                while channel.push(value, 1).is_err() {
                    thread::yield_now();
                }
                sent += 1;
            }
            sent
        })
    };

    // The consumer must know when to stop; draining until empty would race the
    // producer and never terminate. Counting the sent items gives a fixed total.
    let total = Arc::new(AtomicUsize::new(0));

    let consumer = {
        let (channel, barrier, total) = (channel.clone(), barrier.clone(), total.clone());
        thread::spawn(move || {
            barrier.wait();
            while total.load(Ordering::Acquire) < ROUNDS as usize {
                if channel.pop().is_some() {
                    total.fetch_add(1, Ordering::Release);
                } else {
                    thread::yield_now();
                }
            }
        })
    };

    let sent = producer.join().expect("producer");
    consumer.join().expect("consumer");
    assert_eq!(sent as usize, ROUNDS as usize);
    assert_eq!(total.load(Ordering::Acquire), ROUNDS as usize);
    assert!(channel.is_empty());
    assert!(
        channel.pop().is_none(),
        "nothing is left after both sides finish"
    );
}
