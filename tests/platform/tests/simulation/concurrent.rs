//! Concurrent runner contracts at actual SDK/production-host boundaries. These
//! cases protect overlap, budget reservations, wake propagation, cancellation,
//! independent contention checks and callbacks on the same virtual scheduler.
use crate::{campaign_setup, setup};
use snap_platform_tests::{
    runner::{self, Budget, Config, Drive, Host, Timer, TranscriptSummary, Workload},
    simulation::{Action, Failure, Schedule},
    workload::{World, concurrent},
};
use std::{
    cell::Cell,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::Poll,
};

fn schedule() -> Schedule {
    Schedule {
        seed: 19,
        jitter_ms: 20,
        max_events: 100_000,
        max_polls: 100_000,
        ..Default::default()
    }
}

#[test]
fn concurrent_operation_budget_is_global_exact_and_overlaps_real_sdk_calls() {
    for clients in [1, 2, 7, 127] {
        for operations in [0, 1, clients as u64 + 1, 129] {
            let mut campaign =
                campaign_setup::assemble(schedule(), &World::generate(42), clients, false).unwrap();
            let report = runner::run_many(
                &mut campaign.simulation,
                &mut campaign.actors,
                Config {
                    seed: 42,
                    budget: Budget::Operations(operations),
                },
            )
            .unwrap();
            assert_eq!(report.campaign.started, operations);
            assert_eq!(report.campaign.completed, operations);
            let streams = TranscriptSummary::combine(
                &campaign
                    .transcripts
                    .iter()
                    .map(|stream| stream.summary())
                    .collect::<Vec<_>>(),
            );
            assert_eq!(
                streams.sent,
                operations + clients as u64 + 2,
                "setup contains each Connect and two world operations; no hidden retries"
            );
            assert_eq!(
                report
                    .actors
                    .iter()
                    .map(|actor| actor.completed)
                    .sum::<u64>(),
                operations
            );
            let counts: Vec<_> = report.actors.iter().map(|actor| actor.completed).collect();
            assert!(
                counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1,
                "round selection must remain fair"
            );
            if clients > 1 && operations >= clients as u64 {
                let trace = campaign.timeline.trace();
                let mut outstanding = 0;
                let mut overlap = false;
                // Setup is finished before the campaign's first TimerScheduled.
                let start = trace
                    .iter()
                    .position(|record| matches!(record.action, Action::TimerScheduled { .. }))
                    .unwrap();
                for record in &trace[start..] {
                    match record.action {
                        Action::CommandQueued { kind: "invoke", .. } => outstanding += 1,
                        Action::ResponseDelivered {
                            kind: "completed", ..
                        } => outstanding -= 1,
                        _ => {}
                    }
                    overlap |= outstanding >= 2;
                }
                assert!(
                    overlap,
                    "independent clients must queue calls before earlier calls complete"
                );
            }
        }
    }
}

#[test]
fn virtual_timers_share_host_events_and_drop_cancels_pending_deadlines() {
    let (mut simulation, timeline, _) = setup::setup(schedule());
    let clock = simulation.clock();
    let start = clock.now();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let task = simulation.schedule_task(start + 10, Some(10), "maintenance", move |_, _| {
        observed.set(observed.get() + 1)
    });
    assert_eq!(
        simulation.drive(clock.sleep(35), None).unwrap(),
        Drive::Complete(())
    );
    assert_eq!(
        calls.get(),
        3,
        "host tasks must run while the client sleeps"
    );
    assert_eq!(clock.now(), start + 35);
    simulation.cancel_task(task);
    let abandoned = clock.sleep(1_000_000);
    drop(abandoned);
    simulation.finish().unwrap();
    assert!(
        timeline.now() < start + 1_000_000,
        "dropped timers and canceled tasks must not extend teardown"
    );
    assert!(
        timeline
            .trace()
            .iter()
            .any(|record| matches!(record.action, Action::TimerCanceled { .. }))
    );
}

#[test]
fn joining_tasks_does_not_turn_another_childs_wake_into_a_missing_wake() {
    let (mut simulation, _, _) = setup::setup(schedule());
    let missing_polls = Rc::new(Cell::new(0));
    let polls = missing_polls.clone();
    let mut wakes = 0;
    let tasks: Vec<Pin<Box<dyn Future<Output = ()>>>> = vec![
        Box::pin(poll_fn(move |_| {
            polls.set(polls.get() + 1);
            if polls.get() == 1 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })),
        Box::pin(poll_fn(move |context| {
            wakes += 1;
            if wakes == 1 {
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })),
    ];
    assert_eq!(simulation.run(runner::join(tasks)), Err(Failure::Deadlock));
    assert_eq!(missing_polls.get(), 1, "each child needs its own wake");
}

#[test]
fn paused_host_defers_scheduled_callbacks_but_not_client_timers() {
    let (mut simulation, timeline, _) = setup::setup(schedule());
    let calls = Rc::new(Cell::new(0));
    let count = calls.clone();
    let task = simulation.schedule_task(timeline.now() + 5, Some(5), "gate-check", move |_, _| {
        count.set(count.get() + 1)
    });
    simulation.pause_host(true);
    assert_eq!(
        simulation
            .drive(simulation.clock().sleep(20), None)
            .unwrap(),
        Drive::Complete(())
    );
    assert_eq!(calls.get(), 0);
    simulation.pause_host(false);
    simulation.advance_by(5).unwrap();
    assert!(
        calls.get() > 0,
        "scheduled host work must resume when the gate opens"
    );
    simulation.cancel_task(task);
}

#[test]
fn concurrent_campaign_replays_timers_contention_checks_and_commit_faults() {
    for clients in [1, 2, 7] {
        for budget in [Budget::Operations(200), Budget::TimeMs(3_000)] {
            let run = || {
                let mut campaign =
                    campaign_setup::assemble(schedule(), &World::generate(42), clients, true)
                        .unwrap();
                let report = runner::run_many(
                    &mut campaign.simulation,
                    &mut campaign.actors,
                    Config { seed: 42, budget },
                )
                .unwrap();
                let streams: Vec<_> = campaign
                    .transcripts
                    .iter()
                    .map(|stream| stream.summary())
                    .collect();
                assert!(campaign.fault_injections.get() > 0);
                assert!(
                    campaign.server_checks.get() > 0,
                    "server-owned Read must complete during SDK activity"
                );
                assert!(
                    campaign.timeline.trace().iter().any(|record| matches!(
                        record.action,
                        Action::StoreCommit { rejected: true }
                    )),
                    "campaign must hit the backend rejection path"
                );
                if let Budget::TimeMs(duration) = budget {
                    assert!(report.campaign.end_ms >= report.campaign.start_ms + duration);
                    assert_eq!(
                        report.campaign.end_ms - report.campaign.start_ms - duration,
                        report.campaign.overrun_ms
                    );
                    assert!(report.campaign.started - report.campaign.completed <= clients as u64);
                    assert!(
                        campaign
                            .timeline
                            .trace()
                            .iter()
                            .any(|record| matches!(record.action, Action::TimerCanceled { .. }))
                            || report.campaign.started > report.campaign.completed
                    );
                }
                (
                    report,
                    streams,
                    campaign.timeline.events_sha256(),
                    campaign.actors[0].expected_value(),
                    campaign.server_checks.get(),
                )
            };
            assert_eq!(run(), run(), "clients={clients} budget={budget:?}");
        }
    }
}

#[test]
fn contention_oracle_rejects_state_drift_instead_of_adopting_server_results() {
    use snap_platform_tests::{
        cartridge::{Change, Edit, Stop},
        workload::Action as InputAction,
    };
    use snap_transport::{Operation, client::Client};
    let world = World::generate(42);
    let mut campaign = campaign_setup::assemble(schedule(), &world, 2, false).unwrap();
    let mut outside = Client::new(campaign.simulation.open().unwrap());
    campaign
        .simulation
        .drive(
            async {
                outside.connect("alice", "outside").await.unwrap();
                outside
                    .invoke(
                        Change::NAME,
                        serde_json::to_value(Edit {
                            expected: world.value,
                            amount: 1,
                            stop: Stop::Commit,
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            },
            None,
        )
        .unwrap();
    campaign
        .simulation
        .drive(
            runner::join(
                campaign
                    .actors
                    .iter_mut()
                    .enumerate()
                    .map(|(actor, probe)| {
                        Box::pin(probe.execute(concurrent::Input {
                            delay_ms: 0,
                            action: InputAction::Change(Edit {
                                expected: world.value,
                                amount: actor as i64 + 2,
                                stop: Stop::Commit,
                            }),
                        })) as Pin<Box<dyn Future<Output = ()>>>
                    })
                    .collect(),
            ),
            None,
        )
        .unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        campaign_setup::Actor::check_round(&mut campaign.actors)
    }))
    .unwrap_err();
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(message.contains("fresh compare refused without another writer"));
    assert_eq!(campaign.actors[0].expected_value(), world.value);
}

#[test]
fn contention_oracle_rejects_a_fresh_admission_after_the_winner_finished() {
    use snap_platform_tests::{
        cartridge::{Edit, Stop},
        workload::Action as InputAction,
    };
    use snap_transport::{Channel, Command, Error, Event, Response, client::Client, json};
    // An adversarial carrier proxy turns the late writer's legitimate stale
    // refusal into Accepted + declined. A count-only oracle would accept this:
    // one commit won and another application attempt rolled back. The call
    // intervals independently prove that its baseline guard could not pass.
    struct Bypass<C> {
        channel: C,
        poison: bool,
        pending: Option<Response>,
    }
    impl<C: Channel> Channel for Bypass<C> {
        async fn send(&mut self, command: Command) -> Result<(), Error> {
            self.channel.send(command).await
        }
        async fn receive(&mut self) -> Result<Option<Response>, Error> {
            if let Some(response) = self.pending.take() {
                return Ok(Some(response));
            }
            let response = self.channel.receive().await?;
            if self.poison
                && let Some(Response::Event(Event::Completed {
                    id,
                    outcome: Err(Error::Application(error)),
                })) = &response
                && *error == json!("stale")
            {
                self.pending = Some(Response::Event(Event::Completed {
                    id: *id,
                    outcome: Err(Error::Application(json!("declined"))),
                }));
                return Ok(Some(Response::Event(Event::Accepted { id: *id })));
            }
            Ok(response)
        }
    }
    let (mut simulation, _, _) = setup::setup(schedule());
    let world = World::generate(42);
    let mut clients = Vec::new();
    for actor in 0..2 {
        let mut client = Client::new(Bypass {
            channel: simulation.open().unwrap(),
            poison: actor == 1,
            pending: None,
        });
        simulation
            .run(client.connect("alice", &format!("causal-{actor}")))
            .unwrap()
            .unwrap();
        clients.push(client);
    }
    let mut initializer = snap_platform_tests::workload::Probe::new(clients.remove(0));
    simulation.run(initializer.initialize(&world)).unwrap();
    clients.insert(0, initializer.into_client());
    let mut actors =
        concurrent::Probe::actors(clients, simulation.clock(), &world, Default::default());
    simulation
        .run(runner::join(
            actors
                .iter_mut()
                .enumerate()
                .map(|(actor, probe)| {
                    Box::pin(probe.execute(concurrent::Input {
                        delay_ms: if actor == 0 { 0 } else { 1_000 },
                        action: InputAction::Change(Edit {
                            expected: world.value,
                            amount: actor as i64 + 2,
                            stop: if actor == 0 {
                                Stop::Commit
                            } else {
                                Stop::Application
                            },
                        }),
                    })) as Pin<Box<dyn Future<Output = ()>>>
                })
                .collect(),
        ))
        .unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        <concurrent::Probe<_, _> as Workload>::check_round(&mut actors)
    }))
    .unwrap_err();
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(
        message.contains("happens-before"),
        "must reject causal inconsistency, not ordinary transport failure"
    );
}

#[test]
fn concurrent_oracle_runs_on_real_tcp_sqlite_without_virtual_timing_assumptions() {
    use snap_transport::client::Client;
    #[derive(Clone)]
    struct NativeTimer;
    impl Timer for NativeTimer {
        async fn sleep(&self, milliseconds: u64) {
            tokio::time::sleep(std::time::Duration::from_millis(milliseconds)).await;
        }
    }
    struct Native(tokio::runtime::Runtime);
    impl Host for Native {
        type Error = std::convert::Infallible;
        fn now(&self) -> u64 {
            0
        }
        fn drive<F: Future>(
            &mut self,
            future: F,
            deadline_ms: Option<u64>,
        ) -> Result<Drive<F::Output>, Self::Error> {
            assert!(deadline_ms.is_none());
            Ok(Drive::Complete(self.0.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(5), future)
                    .await
                    .unwrap()
            })))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concurrent.sqlite");
    snap_store_sqlite::migrate(&path, &crate::host::migrations()).unwrap();
    let host = crate::host::mount(
        snap_store_sqlite::Sqlite::open(&path).unwrap(),
        Default::default(),
        "concurrent-native".into(),
    )
    .unwrap();
    let mut native = Native(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let server = native.0.block_on(crate::lifecycle::Tcp::start(host));
    let world = World::generate(42);
    let mut clients = Vec::new();
    for actor in 0..2 {
        let mut client = Client::new(native.0.block_on(server.channel()));
        assert_eq!(
            native
                .drive(client.connect("alice", &format!("native-{actor}")), None)
                .unwrap(),
            Drive::Complete(Ok(false))
        );
        clients.push(client);
    }
    let mut initializer = snap_platform_tests::workload::Probe::new(clients.remove(0));
    native.drive(initializer.initialize(&world), None).unwrap();
    clients.insert(0, initializer.into_client());
    let mut actors = concurrent::Probe::actors(clients, NativeTimer, &world, Default::default());
    let report = runner::run_many(
        &mut native,
        &mut actors,
        Config {
            seed: 42,
            budget: Budget::Operations(40),
        },
    )
    .unwrap();
    assert_eq!(report.campaign.completed, 40);
    let value = actors[0].expected_value();
    drop(actors);
    native.0.block_on(server.stop());
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    for table in snap_platform_tests::cartridge::TABLES {
        store.load(table).unwrap();
    }
    assert_eq!(
        store
            .inspect(
                "concurrent persisted rows",
                snap_platform_tests::cartridge::read
            )
            .unwrap(),
        [value, value]
    );
}
