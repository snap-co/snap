//! Simulator fidelity and scheduling contracts. The cartridge still supplies the
//! independent oracle; real and simulated driver agreement alone is insufficient.
#[path = "../support/campaign.rs"]
mod campaign_setup;
#[path = "simulation/concurrent.rs"]
mod concurrent_contracts;
#[path = "../support/host.rs"]
mod host;
#[path = "support/lifecycle.rs"]
mod lifecycle;
#[path = "../support/tcp_sqlite.rs"]
mod native;
#[path = "simulation/runner.rs"]
mod runner_contracts;
#[path = "../support/simulation.rs"]
mod setup;

use snap_platform_tests::{
    cartridge::{Change, Edit, Read, Stop},
    journey::{self, Observation},
    simulation::{Action, Failure, Schedule},
};
use snap_transport::{
    Channel, Command, Error, Event, Operation,
    client::{Client, Pump},
    json,
};
use std::future::Future;

fn collect(observation: Observation<'_>, events: &mut Vec<Event>) {
    if let Observation::Received { observation, .. } = observation {
        events.push(match observation {
            Pump::Accepted { id } => Event::Accepted { id: *id },
            Pump::Completed { id, outcome } => Event::Completed {
                id: *id,
                outcome: outcome.clone(),
            },
            other => panic!("unexpected cartridge observation {other:?}"),
        });
    }
}

#[tokio::test]
async fn simulated_journey_matches_real_tcp_sqlite_under_seeded_delays() {
    let directory = tempfile::tempdir().unwrap();
    let mut real = Vec::new();
    assert_eq!(
        native::run(&directory.path().join("reference.sqlite"), |observation| {
            collect(observation, &mut real)
        })
        .await
        .unwrap(),
        [5, 5]
    );
    for seed in [0, 42, u64::MAX] {
        let schedule = Schedule {
            seed,
            jitter_ms: 20,
            ..Default::default()
        };
        let mut replay_trace = None;
        for _ in 0..2 {
            let (mut simulation, timeline, _) = setup::setup(schedule);
            let channel = simulation.open().unwrap();
            let mut observed = Vec::new();
            simulation
                .run(async {
                    let mut client = Client::new(channel);
                    assert!(!client.connect("alice", "plumbing-client").await.unwrap());
                    assert_eq!(
                        journey::run(&mut client, |observation| collect(
                            observation,
                            &mut observed
                        ))
                        .await
                        .unwrap(),
                        5
                    );
                })
                .unwrap();
            assert_eq!(
                observed, real,
                "SDK observations disagree for schedule seed {seed}"
            );
            assert!(timeline.now() > 0);
            let trace = timeline.trace();
            assert!(
                trace
                    .iter()
                    .any(|record| matches!(record.action, Action::StoreCommit { rejected: false }))
            );
            if let Some(expected) = &replay_trace {
                assert_eq!(&trace, expected, "schedule replay diverged for seed {seed}");
            }
            replay_trace = Some(trace);
        }
    }
}

#[test]
fn send_only_queues_and_acceptance_can_arrive_before_execution() {
    let (mut simulation, timeline, _) = setup::setup(Schedule::default());
    let mut channel = simulation.open().unwrap();
    let start = timeline.now();
    ready(channel.send(Command::Connect {
        bearer: "alice".into(),
        client_id: "split".into(),
    }))
    .unwrap();
    assert_eq!(timeline.now(), start, "queueing cannot advance host time");
    assert!(
        poll_once(channel.receive()).is_none(),
        "an empty live stream is not EOF"
    );
    assert!(simulation.step().unwrap());
    assert_eq!(
        timeline.now(),
        start + 2,
        "jump directly to command delivery"
    );
    assert!(
        poll_once(channel.receive()).is_none(),
        "receipt must not collapse into host submission"
    );
    assert!(simulation.step().unwrap());
    assert!(
        timeline
            .trace()
            .iter()
            .any(|record| record.at_ms == start + 3
                && matches!(
                    record.action,
                    Action::CommandSubmitted {
                        kind: "connect",
                        ..
                    }
                ))
    );
    // Admission can synchronously load cold rows and advance Store time.
    let submitted = timeline.now();
    assert!(simulation.step().unwrap());
    assert_eq!(
        timeline.now(),
        submitted + 2,
        "jump directly to response delivery: {:?}",
        timeline.trace()
    );
    assert_eq!(
        ready(channel.receive()).unwrap(),
        Some(snap_transport::Response::Attached { resumed: false })
    );
    let mut client = Client::new(channel);
    let before = timeline.trace().len();
    // Poll begin without driving the simulation: handoff must complete immediately.
    let id = ready(client.begin(Read::NAME, json!(null))).unwrap();
    assert!(
        timeline.trace()[before..]
            .iter()
            .all(|record| matches!(record.action, Action::CommandQueued { .. }))
    );
    assert_eq!(
        simulation.run(client.pump()).unwrap().unwrap(),
        Pump::Accepted { id }
    );
    // run drains queued events; use the trace to distinguish publication/delivery.
    let trace = timeline.trace();
    let accepted_at = trace
        .iter()
        .find_map(|record| match record.action {
            Action::ResponseDelivered {
                kind: "accepted",
                id: Some(observed),
                ..
            } if observed == id => Some(record.at_ms),
            _ => None,
        })
        .unwrap();
    let completed_at = trace
        .iter()
        .find_map(|record| match record.action {
            Action::ResponsePublished {
                kind: "completed",
                id: Some(observed),
                ..
            } if observed == id => Some(record.at_ms),
            _ => None,
        })
        .unwrap();
    assert!(
        accepted_at < completed_at,
        "acceptance was held until execution finished"
    );
    assert_eq!(
        simulation.run(client.pump()).unwrap().unwrap(),
        Pump::Completed {
            id,
            outcome: Ok(json!([0, 0]))
        }
    );
}

#[test]
fn simulated_commit_rejection_preserves_rows_and_never_retries() {
    let (mut simulation, timeline, faults) = setup::setup(Schedule::default());
    let mut client = Client::new(simulation.open().unwrap());
    simulation
        .run(client.connect("alice", "fault"))
        .unwrap()
        .unwrap();
    faults.reject_next();
    let edit = Edit {
        expected: 0,
        amount: 9,
        stop: Stop::Commit,
    };
    let input = serde_json::to_value(&edit).unwrap();
    assert_eq!(
        simulation
            .run(client.invoke(Change::NAME, input.clone()))
            .unwrap(),
        Err(Error::Unavailable)
    );
    assert_eq!(
        simulation
            .run(client.invoke(Read::NAME, json!(null)))
            .unwrap()
            .unwrap(),
        json!([0, 0])
    );
    simulation.finish().unwrap();
    assert_eq!(
        timeline
            .trace()
            .iter()
            .filter(|record| matches!(record.action, Action::StoreCommit { rejected: true }))
            .count(),
        1
    );
    assert_eq!(
        simulation
            .run(client.invoke(Change::NAME, input))
            .unwrap()
            .unwrap(),
        json!([9, 9])
    );
}

#[test]
fn observer_loss_does_not_cancel_accepted_work_or_redirect_completion() {
    let (mut simulation, _, _) = setup::setup(Schedule::default());
    let channel = simulation.open().unwrap();
    let peer = channel.peer();
    let mut client = Client::new(channel);
    simulation
        .run(client.connect("alice", "loss"))
        .unwrap()
        .unwrap();
    let id = ready(client.begin(
        Change::NAME,
        json!({"expected":0,"amount":7,"stop":"Commit"}),
    ))
    .unwrap();
    // Stop precisely at acceptance delivery, before the scheduled execution.
    loop {
        assert!(simulation.step().unwrap());
        if let Some(observation) = poll_once(client.pump()) {
            assert_eq!(observation.unwrap(), Pump::Accepted { id });
            break;
        }
    }
    simulation.disconnect(peer);
    assert!(simulation.step().unwrap());
    assert_eq!(ready(client.pump()), Err(Error::Unavailable));
    // Reattach before the old accepted operation's scheduled execution.
    client.replace_channel(simulation.open().unwrap());
    assert!(
        simulation
            .run(client.connect("alice", "loss"))
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        simulation
            .run(client.invoke(Read::NAME, json!(null)))
            .unwrap()
            .unwrap(),
        json!([7, 7])
    );
    assert_eq!(
        client.outstanding(),
        1,
        "lost completion must not resolve on the replacement link"
    );
}

#[test]
fn stalled_clients_and_excessive_schedules_fail_without_sleeping() {
    let (mut simulation, _, _) = setup::setup(Schedule::default());
    let mut channel = simulation.open().unwrap();
    assert_eq!(simulation.run(channel.receive()), Err(Failure::Deadlock));
    let (mut simulation, _, _) = setup::setup(Schedule {
        max_events: 0,
        ..Default::default()
    });
    let mut client = Client::new(simulation.open().unwrap());
    assert_eq!(
        simulation.run(client.connect("alice", "limit")),
        Err(Failure::EventLimit)
    );
    let (mut simulation, _, _) = setup::setup(Schedule {
        max_time_ms: 1,
        ..Default::default()
    });
    let mut client = Client::new(simulation.open().unwrap());
    assert_eq!(
        simulation.run(client.connect("alice", "limit")),
        Err(Failure::TimeLimit)
    );
}

#[test]
fn concurrent_peers_do_not_share_outputs_or_bypass_compare_guards() {
    for seed in [1, 9, 42] {
        let (mut simulation, _, _) = setup::setup(Schedule {
            seed,
            jitter_ms: 30,
            ..Default::default()
        });
        let mut left = Client::new(simulation.open().unwrap());
        let mut right = Client::new(simulation.open().unwrap());
        simulation
            .run(left.connect("alice", "left-client"))
            .unwrap()
            .unwrap();
        simulation
            .run(right.connect("alice", "right-client"))
            .unwrap()
            .unwrap();
        let (a, b) = simulation
            .run(futures_util::future::join(
                left.invoke(
                    Change::NAME,
                    json!({"expected":0,"amount":1,"stop":"Commit"}),
                ),
                right.invoke(
                    Change::NAME,
                    json!({"expected":0,"amount":2,"stop":"Commit"}),
                ),
            ))
            .unwrap();
        let expected = match (a, b) {
            (Ok(a), Err(error)) => {
                assert_eq!(a, json!([1, 1]));
                assert_eq!(error, Error::Application(json!("stale")));
                1
            }
            (Err(error), Ok(b)) => {
                assert_eq!(b, json!([2, 2]));
                assert_eq!(error, Error::Application(json!("stale")));
                2
            }
            outcomes => panic!("exactly one competing compare must commit: {outcomes:?}"),
        };
        assert_eq!(
            simulation
                .run(left.invoke(Read::NAME, json!(null)))
                .unwrap()
                .unwrap(),
            json!([expected, expected])
        );
        assert_eq!(
            simulation
                .run(right.invoke(Read::NAME, json!(null)))
                .unwrap()
                .unwrap(),
            json!([expected, expected])
        );
    }
}

#[test]
fn pipelined_commands_and_observations_remain_fifo_under_jitter() {
    for seed in 0..16 {
        let (mut simulation, _, _) = setup::setup(Schedule {
            seed,
            jitter_ms: 50,
            ..Default::default()
        });
        let mut client = Client::new(simulation.open().unwrap());
        simulation
            .run(client.connect("alice", "pipeline"))
            .unwrap()
            .unwrap();
        let first = ready(client.begin(
            Change::NAME,
            json!({"expected":0,"amount":3,"stop":"Commit"}),
        ))
        .unwrap();
        let second = ready(client.begin(
            Change::NAME,
            json!({"expected":3,"amount":2,"stop":"Commit"}),
        ))
        .unwrap();
        let observed = simulation
            .run(async {
                let mut observations = Vec::new();
                for _ in 0..4 {
                    observations.push(client.pump().await.unwrap());
                }
                observations
            })
            .unwrap();
        assert_eq!(
            observed,
            vec![
                Pump::Accepted { id: first },
                Pump::Completed {
                    id: first,
                    outcome: Ok(json!([3, 3]))
                },
                Pump::Accepted { id: second },
                Pump::Completed {
                    id: second,
                    outcome: Ok(json!([5, 5]))
                },
            ],
            "per-link FIFO broken for schedule seed {seed}"
        );
    }
}

#[test]
fn explicit_teardown_distinguishes_disconnect_from_logical_close() {
    for close in [false, true] {
        let (mut simulation, _, _) = setup::setup(Schedule::default());
        let mut client = Client::new(simulation.open().unwrap());
        simulation
            .run(client.connect("alice", "teardown"))
            .unwrap()
            .unwrap();
        if close {
            assert_eq!(
                simulation.run(client.close()).unwrap(),
                Err(Error::Unavailable)
            );
        } else {
            assert_eq!(
                simulation.run(client.disconnect()).unwrap(),
                Err(Error::Unavailable)
            );
        }
        // Native carrier teardown closes the socket without a Detached reply.
        // The simulator must not invent an acknowledgement from the host.
        assert_eq!(
            simulation.run(client.pump()).unwrap(),
            Err(Error::Unavailable)
        );
        client.replace_channel(simulation.open().unwrap());
        assert_eq!(
            simulation
                .run(client.connect("alice", "teardown"))
                .unwrap()
                .unwrap(),
            !close
        );
    }
}

#[test]
fn simulated_store_runs_all_shared_store_contracts() {
    for case in [
        snap_platform_tests::store::secondary_indexes_and_negative_results_change_with_the_commit,
        snap_platform_tests::store::unique_index_failure_discards_earlier_statements_and_allows_the_next_operation,
        snap_platform_tests::store::cross_module_constraints_roll_back_every_write_including_memory,
        snap_platform_tests::store::caught_miss_discards_writes_without_loading_or_retrying,
        snap_platform_tests::store::cold_insert_does_not_claim_other_keys_or_a_complete_index,
        snap_platform_tests::store::committed_changes_coalesce_and_publish_only_the_net_state,
        snap_platform_tests::store::releasing_residency_does_not_delete_rows_or_claim_complete_indexes,
        snap_platform_tests::store::mutation_programs_replay_ordered_partial_updates_without_handlers,
    ] {
        let catalog = snap_platform_tests::store::catalog();
        let timeline = snap_platform_tests::simulation::Timeline::new(Schedule::default());
        let backend = snap_platform_tests::simulation::Store::new(catalog.clone(), timeline).unwrap();
        let mut store = snap_store::Store::new(catalog, backend).unwrap();
        case(&mut store);
    }
}

fn poll_once<F: std::future::Future>(future: F) -> Option<F::Output> {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    match std::pin::pin!(future).as_mut().poll(&mut context) {
        std::task::Poll::Ready(result) => Some(result),
        std::task::Poll::Pending => None,
    }
}
fn ready<F: std::future::Future>(future: F) -> F::Output {
    poll_once(future).expect("queue-only send must not wait for host execution")
}

#[tokio::test]
async fn tcp_refusal_policy_matches_simulation() {
    let store = snap_store_sqlite::Sqlite::memory(&host::migrations()).unwrap();
    let real = lifecycle::Tcp::start(
        host::mount(store, Default::default(), "refusal-boot".into()).unwrap(),
    )
    .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        snap_platform_tests::transport::refusal_ends_physical_connection(real.channel().await),
    )
    .await
    .unwrap();
    real.stop().await;
    let (mut simulation, _, _) = setup::setup(Schedule::default());
    let channel = simulation.open().unwrap();
    simulation
        .run(snap_platform_tests::transport::refusal_ends_physical_connection(channel))
        .unwrap();
}

#[tokio::test]
async fn retirement_drains_final_frames_and_seals_late_output_in_both_setups() {
    for (final_output, terminal) in [(true, true), (true, false), (false, false)] {
        let host = lifecycle::Retirement::new(final_output, terminal);
        let output = snap_transport::runtime::Loop::output(&host, 1).unwrap();
        let real = lifecycle::Tcp::start(host).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            snap_platform_tests::transport::retirement_drains_output_before_loss(
                real.channel().await,
                final_output,
            ),
        )
        .await
        .unwrap();
        // Publish only after the carrier has reported physical loss. A fixture
        // step racing the native retirement sweep is not post-retirement output.
        output.push_back(snap_transport::Response::Global {
            kind: "late".into(),
            input: json!(true),
        });
        assert!(
            output.is_empty(),
            "native retired outbox must reject later publication"
        );
        real.stop().await;
        for seed in [0, 42, u64::MAX] {
            let timeline = snap_platform_tests::simulation::Timeline::new(Schedule {
                seed,
                jitter_ms: 50,
                ..Default::default()
            });
            let host = lifecycle::Retirement::new(final_output, terminal);
            let output = snap_transport::runtime::Loop::output(&host, 1).unwrap();
            let mut simulation = snap_platform_tests::simulation::Simulation::new(host, timeline);
            let channel = simulation.open().unwrap();
            simulation
                .run(
                    snap_platform_tests::transport::retirement_drains_output_before_loss(
                        channel,
                        final_output,
                    ),
                )
                .unwrap();
            output.push_back(snap_transport::Response::Global {
                kind: "late".into(),
                input: json!(true),
            });
            assert!(
                output.is_empty(),
                "simulated retired outbox must reject later publication"
            );
        }
    }
}

#[test]
fn self_waking_future_progresses_without_a_network_event() {
    let (mut simulation, _, _) = setup::setup(Schedule::default());
    let mut first = true;
    assert_eq!(
        simulation.run(std::future::poll_fn(|context| {
            if std::mem::take(&mut first) {
                context.waker().wake_by_ref();
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(7)
            }
        })),
        Ok(7)
    );
}

#[test]
fn missing_receive_wakeup_is_not_hidden_by_unconditional_polling() {
    let (mut simulation, _, _) = setup::setup(Schedule::default());
    let mut channel = simulation.open().unwrap();
    ready(channel.send(Command::Connect {
        bearer: "alice".into(),
        client_id: "wake".into(),
    }))
    .unwrap();
    let mut receive = std::pin::pin!(channel.receive());
    assert_eq!(
        simulation.run(std::future::poll_fn(|_| {
            receive
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        })),
        Err(Failure::Deadlock)
    );
}

#[test]
fn nonterminal_refusal_policy_allows_retry_on_the_same_link() {
    use snap_platform_tests::simulation::{CarrierPolicy, Simulation, Store, Timeline};
    let timeline = Timeline::new(Schedule::default());
    let catalog = host::migrations()[0]
        .apply(&snap_store::Catalog::default())
        .unwrap();
    let backend = Store::new(catalog.clone(), timeline.clone()).unwrap();
    let host = host::mount(
        snap_store::Store::new(catalog, backend).unwrap(),
        Default::default(),
        "web-policy".into(),
    )
    .unwrap();
    let mut simulation = Simulation::with_policy(
        host,
        timeline,
        CarrierPolicy {
            terminal_refusals: false,
            ..Default::default()
        },
    );
    let mut client = Client::new(simulation.open().unwrap());
    assert_eq!(
        simulation.run(client.connect("invalid", "retry")).unwrap(),
        Err(Error::InvalidBearer)
    );
    assert_eq!(
        simulation.run(client.connect("alice", "retry")).unwrap(),
        Ok(false)
    );
}

#[tokio::test]
async fn queued_commands_cannot_enter_admission_after_gate_independent_teardown() {
    use snap_platform_tests::simulation::{Simulation, Timeline};
    use snap_transport::{
        carrier::{Connection, Dispatch, Submission},
        native::driver::{Dispatcher, Shared},
    };
    for overflow in [false, true] {
        let command = || Command::Connect {
            bearer: "unused".into(),
            client_id: "held".into(),
        };
        let host = lifecycle::Retirement::new(false, false);
        let submissions = host.submissions.clone();
        let shared = Shared::new(host);
        let endpoint = Dispatcher::tcp(shared.clone(), None)
            .open(None, usize::MAX)
            .await
            .unwrap();
        let gate = shared.host.lock().unwrap();
        let count = if overflow { 1024 } else { 1 };
        for _ in 0..count {
            assert_eq!(endpoint.submit(command(), 1), Ok(Submission::Queued));
        }
        if overflow {
            assert_eq!(endpoint.submit(command(), 1), Err(Error::Capacity));
        } else {
            assert_eq!(
                endpoint.submit(Command::Close, 0),
                Ok(Submission::CloseSocket)
            );
        }
        // The TCP carrier drops the physical endpoint on handoff overflow too.
        endpoint.disconnect();
        assert!(endpoint.receive().is_none());
        drop(gate);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !endpoint.retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *submissions.lock().unwrap(),
            0,
            "native closed command entered admission"
        );

        let host = lifecycle::Retirement::new(false, false);
        let submissions = host.submissions.clone();
        let timeline = Timeline::new(Schedule::default());
        let mut simulation = Simulation::new(host, timeline.clone());
        let mut channel = simulation.open().unwrap();
        simulation.pause_host(true);
        for _ in 0..count {
            ready(channel.send(command())).unwrap();
        }
        if overflow {
            ready(channel.send(command())).unwrap();
        } else {
            ready(channel.send(Command::Close)).unwrap();
        }
        simulation.finish().unwrap();
        assert!(
            timeline.trace().iter().any(|record| matches!(
                record.action,
                Action::CommandDelivered {
                    kind: "connect",
                    ..
                }
            )),
            "carrier receipt must continue while the gate is held"
        );
        assert_eq!(*submissions.lock().unwrap(), 0);
        assert_eq!(
            ready(channel.receive()),
            Err(Error::Unavailable),
            "carrier teardown cannot wait for host submission"
        );
        simulation.pause_host(false);
        simulation.finish().unwrap();
        assert_eq!(
            *submissions.lock().unwrap(),
            0,
            "simulated closed command entered admission"
        );
    }
}

#[tokio::test]
async fn idle_revocation_retires_a_waiting_observer_in_both_setups() {
    use snap_platform_tests::simulation::{Simulation, Store, Timeline};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    fn revocable<B: snap_store::Backend>(
        backend: B,
        valid: Arc<AtomicBool>,
    ) -> snap_transport::host::Blocking<B> {
        snap_transport::host::Blocking::new(
            snap_store::Store::new(Default::default(), backend).unwrap(),
            (),
            Default::default(),
            Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
                move |_, _| {
                    if valid.load(Ordering::SeqCst) {
                        Ok("alice".into())
                    } else {
                        Err(snap_store::Error::NotFound)
                    }
                },
            ))),
            Default::default(),
            "idle-boot".into(),
        )
    }
    let valid = Arc::new(AtomicBool::new(true));
    let real = lifecycle::Tcp::start(revocable(
        snap_store::memory::Memory::new(Default::default()).unwrap(),
        valid.clone(),
    ))
    .await;
    let mut client = Client::new(real.channel().await);
    client.connect("alice", "idle").await.unwrap();
    valid.store(false, Ordering::SeqCst);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), client.pump())
            .await
            .unwrap(),
        Err(Error::Unavailable)
    );
    real.stop().await;

    let valid = Arc::new(AtomicBool::new(true));
    let timeline = Timeline::new(Schedule::default());
    let backend = Store::new(Default::default(), timeline.clone()).unwrap();
    let mut simulation = Simulation::new(revocable(backend, valid.clone()), timeline.clone());
    let mut client = Client::new(simulation.open().unwrap());
    simulation
        .run(client.connect("alice", "idle"))
        .unwrap()
        .unwrap();
    valid.store(false, Ordering::SeqCst);
    assert!(poll_once(client.pump()).is_none());
    let before = timeline.now();
    simulation.advance_by(500).unwrap();
    assert_eq!(timeline.now(), before + 500);
    assert_eq!(
        simulation.run(client.pump()).unwrap(),
        Err(Error::Unavailable)
    );
}

#[test]
fn endless_self_wakes_hit_a_poll_budget_without_advancing_time() {
    let (mut simulation, timeline, _) = setup::setup(Schedule {
        max_polls: 3,
        ..Default::default()
    });
    let start = timeline.now();
    assert_eq!(
        simulation.run(std::future::poll_fn::<(), _>(|context| {
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        })),
        Err(Failure::PollLimit)
    );
    assert_eq!(timeline.now(), start);
}
