//! Runner ownership: exact action counts, partial calls at a horizon, genuine
//! watchdog failures, bounded recording and application-independent SDK driving.
use crate::setup;
use snap_platform_tests::{
    cartridge::Read,
    runner::{self, Budget, Config, Random, Transcript, Workload},
    simulation::{Failure, Schedule},
};
use snap_transport::{Channel, Operation, client::Client, json};

struct Reads<C> {
    client: Client<C>,
    completed: u64,
}
impl<C: Channel> Workload for Reads<C> {
    type Action = ();
    fn generate(&mut self, _: &mut Random) {}
    async fn execute(&mut self, _: ()) {
        assert_eq!(
            self.client.invoke(Read::NAME, json!(null)).await.unwrap(),
            json!([0, 0])
        );
        self.completed += 1;
    }
}

#[test]
fn operation_budget_counts_completed_sdk_calls_without_extra_work() {
    for count in [0, 1, 17] {
        let (mut simulation, _, _) = setup::setup(Schedule::default());
        let transcript = Transcript::default();
        let mut client = Client::new(transcript.channel(simulation.open().unwrap()));
        simulation
            .run(client.connect("alice", "budget"))
            .unwrap()
            .unwrap();
        let mut workload = Reads {
            client,
            completed: 0,
        };
        let report = runner::run(
            &mut simulation,
            &mut workload,
            Config {
                seed: 42,
                budget: Budget::Operations(count),
            },
        )
        .unwrap();
        assert_eq!(report.started, count);
        assert_eq!(report.completed, count);
        assert_eq!(workload.completed, count);
        assert_eq!(
            transcript.summary().sent,
            count + 1,
            "bootstrap Connect is recorded but not a workload operation"
        );
        assert_eq!(workload.client.outstanding(), 0);
        if count == 1 {
            // Independent SHA-256 vectors over Connect + Invoke and Attached +
            // Accepted + Completed. A self-comparison cannot guard the v1 encoding.
            let streams = transcript.summary();
            assert_eq!(
                runner::hex(&streams.commands_sha256),
                "82f59a4aff6e352442bf2af67a0897058f7982c77ad6027513185224b95313b0"
            );
            assert_eq!(
                runner::hex(&streams.observations_sha256),
                "f5f254f47d0cda14db5320ff111df83148071753d17f14e79e718015153546cb"
            );
        }
    }
}

#[test]
fn time_horizon_stops_between_events_without_draining_or_canceling_a_call() {
    for duration in [0, 1, 50] {
        let (mut simulation, timeline, _) = setup::setup(Schedule {
            command_ms: 100,
            ..Default::default()
        });
        let transcript = Transcript::default();
        let mut client = Client::new(transcript.channel(simulation.open().unwrap()));
        simulation
            .run(client.connect("alice", "horizon"))
            .unwrap()
            .unwrap();
        let mut workload = Reads {
            client,
            completed: 0,
        };
        let report = runner::run(
            &mut simulation,
            &mut workload,
            Config {
                seed: 42,
                budget: Budget::TimeMs(duration),
            },
        )
        .unwrap();
        assert_eq!(report.end_ms, report.start_ms + duration);
        assert_eq!(report.completed, 0);
        assert_eq!(report.started, u64::from(duration > 0));
        assert_eq!(report.overrun_ms, 0);
        assert_eq!(workload.client.outstanding(), usize::from(duration > 0));
        assert_eq!(timeline.now(), report.end_ms);
        assert_eq!(transcript.summary().sent, 1 + report.started);
    }
}

#[test]
fn synchronous_store_overrun_is_reported_instead_of_clamping_the_clock() {
    let (mut simulation, _, _) = setup::setup(Schedule {
        load_ms: 100,
        ..Default::default()
    });
    let mut client = Client::new(simulation.open().unwrap());
    simulation
        .run(client.connect("alice", "overrun"))
        .unwrap()
        .unwrap();
    let mut workload = Reads {
        client,
        completed: 0,
    };
    let report = runner::run(
        &mut simulation,
        &mut workload,
        Config {
            seed: 1,
            budget: Budget::TimeMs(20),
        },
    )
    .unwrap();
    assert_eq!(report.started, 1);
    assert_eq!(report.completed, 0);
    assert!(
        report.overrun_ms >= 100,
        "cold SDK read must finish its synchronous backend loads"
    );
    assert_eq!(
        report.end_ms,
        report.deadline_ms.unwrap() + report.overrun_ms
    );
}

#[test]
fn recent_trace_capacity_does_not_change_execution_or_full_event_fingerprints() {
    let mut reference = None;
    for capacity in [0, 3, 10_000] {
        let (mut simulation, timeline, _) = setup::setup(Schedule {
            trace_capacity: capacity,
            ..Default::default()
        });
        let mut client = Client::new(simulation.open().unwrap());
        simulation
            .run(client.connect("alice", "trace"))
            .unwrap()
            .unwrap();
        let mut workload = Reads {
            client,
            completed: 0,
        };
        let report = runner::run(
            &mut simulation,
            &mut workload,
            Config {
                seed: 1,
                budget: Budget::Operations(17),
            },
        )
        .unwrap();
        assert!(timeline.trace().len() <= capacity);
        if capacity < 10_000 {
            assert!(timeline.discarded_records() > 0);
        }
        let actual = (report, timeline.events_sha256());
        if let Some(expected) = &reference {
            assert_eq!(&actual, expected);
        } else {
            reference = Some(actual);
        }
    }
}

#[test]
fn safety_watchdogs_are_failures_not_successful_campaign_budgets() {
    let (mut simulation, _, _) = setup::setup(Schedule {
        max_events: 20,
        ..Default::default()
    });
    let mut client = Client::new(simulation.open().unwrap());
    simulation
        .run(client.connect("alice", "watchdog"))
        .unwrap()
        .unwrap();
    let mut workload = Reads {
        client,
        completed: 0,
    };
    assert_eq!(
        runner::run(
            &mut simulation,
            &mut workload,
            Config {
                seed: 1,
                budget: Budget::Operations(100)
            }
        ),
        Err(Failure::EventLimit)
    );
}

#[test]
fn named_random_streams_have_independent_replay_and_exclusive_bounds() {
    let mut workload = Random::stream(42, "workload");
    let mut reference = Random::stream(42, "workload");
    let mut schedule = Random::stream(42, "schedule");
    let first_workload = reference.next_u64();
    assert_ne!(first_workload, schedule.next_u64());
    for _ in 0..100 {
        schedule.next_u64();
    }
    assert_eq!(workload.next_u64(), first_workload);
    for _ in 0..100 {
        assert_eq!(workload.next_u64(), reference.next_u64());
    }
    for bound in [1, 3, u64::MAX] {
        for _ in 0..100 {
            assert!(workload.below(bound) < bound);
        }
    }
}

fn campaign(
    seed: u64,
    budget: Budget,
    schedule_seed: u64,
    world: snap_platform_tests::workload::World,
) -> (runner::Report, runner::TranscriptSummary, [u8; 32], i64) {
    use snap_platform_tests::workload::Probe;
    let (mut simulation, timeline, _) = setup::setup(Schedule {
        seed: schedule_seed,
        jitter_ms: 20,
        trace_capacity: 16,
        ..Default::default()
    });
    let transcript = Transcript::default();
    let mut client = Client::new(transcript.channel(simulation.open().unwrap()));
    simulation
        .run(client.connect("alice", "generated"))
        .unwrap()
        .unwrap();
    let mut workload = Probe::new(client);
    simulation.run(workload.initialize(&world)).unwrap();
    let report = runner::run(&mut simulation, &mut workload, Config { seed, budget }).unwrap();
    assert!(timeline.trace().len() <= 16);
    (
        report,
        transcript.summary(),
        timeline.events_sha256(),
        workload.expected_value(),
    )
}

#[test]
fn generated_world_and_sdk_campaign_replay_under_both_budgets() {
    use snap_platform_tests::workload::World;
    for seed in [0, 42, u64::MAX] {
        for budget in [Budget::Operations(100), Budget::TimeMs(500)] {
            let world = World::generate(seed);
            let expected = campaign(seed, budget, 17, world.clone());
            assert_eq!(
                campaign(seed, budget, 17, world),
                expected,
                "seed={seed} budget={budget:?}"
            );
        }
    }
}

#[test]
fn replay_fingerprints_detect_payload_changes_and_distinguish_schedule_from_workload() {
    use snap_platform_tests::workload::World;
    let world = World::generate(42);
    let (report, streams, events, value) = campaign(42, Budget::Operations(100), 17, world.clone());
    let (rescheduled, rescheduled_streams, rescheduled_events, rescheduled_value) =
        campaign(42, Budget::Operations(100), 91, world.clone());
    assert_eq!(rescheduled.actions_sha256, report.actions_sha256);
    assert_eq!(rescheduled_streams, streams);
    assert_eq!(rescheduled_value, value);
    assert_ne!(
        rescheduled_events, events,
        "request equality must not mask changed virtual scheduling"
    );
    let (_, changed_streams, _, _) = campaign(
        42,
        Budget::Operations(100),
        17,
        World {
            value: world.value + 1,
        },
    );
    assert_eq!(changed_streams.sent, streams.sent);
    assert_ne!(
        changed_streams.commands_sha256, streams.commands_sha256,
        "same count and operation names with changed payloads must differ"
    );
    assert_ne!(
        changed_streams.observations_sha256,
        streams.observations_sha256
    );
}

#[test]
fn generated_workload_keeps_its_independent_model_when_server_state_disagrees() {
    use snap_platform_tests::{
        cartridge::{Change, Edit, Stop},
        workload::{Action, Probe, World},
    };
    let (mut simulation, _, _) = setup::setup(Schedule::default());
    let mut client = Client::new(simulation.open().unwrap());
    simulation
        .run(client.connect("alice", "model"))
        .unwrap()
        .unwrap();
    let mut workload = Probe::new(client);
    let world = World::generate(42);
    simulation.run(workload.initialize(&world)).unwrap();
    // Change real authority outside the workload's ledger. The subsequent SDK
    // read must panic rather than adopting the response as its expected state.
    let mut outside = Client::new(simulation.open().unwrap());
    simulation
        .run(outside.connect("alice", "outside"))
        .unwrap()
        .unwrap();
    simulation
        .run(
            outside.invoke(
                Change::NAME,
                serde_json::to_value(Edit {
                    expected: world.value,
                    amount: 1,
                    stop: Stop::Commit,
                })
                .unwrap(),
            ),
        )
        .unwrap()
        .unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        simulation.run(workload.execute(Action::Read))
    }));
    let message = panic.expect_err("independent state mismatch must terminate the campaign");
    let message = message.downcast_ref::<String>().unwrap();
    assert!(
        message.contains("cartridge invariant failed"),
        "must fail for the invariant, not a scheduling error"
    );
    assert_eq!(workload.expected_value(), world.value);
}

#[test]
fn generated_sdk_workload_is_portable_to_real_tcp_sqlite() {
    use snap_platform_tests::{
        runner::{Drive, Host},
        workload::{Probe, World},
    };
    struct Native(tokio::runtime::Runtime);
    impl Host for Native {
        type Error = std::convert::Infallible;
        fn now(&self) -> u64 {
            0
        }
        fn drive<F: std::future::Future>(
            &mut self,
            future: F,
            deadline_ms: Option<u64>,
        ) -> Result<Drive<F::Output>, Self::Error> {
            assert!(
                deadline_ms.is_none(),
                "native fixture supports operation budgets, not deterministic time"
            );
            Ok(Drive::Complete(self.0.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(2), future)
                    .await
                    .unwrap()
            })))
        }
    }
    for seed in [0, 42, u64::MAX] {
        let world = World::generate(seed);
        let config = Config {
            seed,
            budget: Budget::Operations(50),
        };
        let (expected, streams, _, value) = campaign(seed, config.budget, 17, world.clone());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("campaign.sqlite");
        snap_store_sqlite::migrate(&path, &crate::host::migrations()).unwrap();
        let store = snap_store_sqlite::Sqlite::open(&path).unwrap();
        let host = crate::host::mount(store, Default::default(), "native-campaign".into()).unwrap();
        let mut native = Native(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let server = native.0.block_on(crate::lifecycle::Tcp::start(host));
        let transcript = Transcript::default();
        let mut client = Client::new(transcript.channel(native.0.block_on(server.channel())));
        assert_eq!(
            native
                .drive(client.connect("alice", "generated"), None)
                .unwrap(),
            Drive::Complete(Ok(false))
        );
        let mut workload = Probe::new(client);
        native.drive(workload.initialize(&world), None).unwrap();
        let actual = runner::run(&mut native, &mut workload, config).unwrap();
        assert_eq!(actual.completed, expected.completed);
        assert_eq!(actual.actions_sha256, expected.actions_sha256);
        assert_eq!(transcript.summary(), streams);
        assert_eq!(workload.expected_value(), value);
        drop(workload);
        native.0.block_on(server.stop());
        let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
        for table in snap_platform_tests::cartridge::TABLES {
            store.load(table).unwrap();
        }
        assert_eq!(
            store
                .inspect("persisted campaign", snap_platform_tests::cartridge::read)
                .unwrap(),
            [value, value]
        );
    }
}
