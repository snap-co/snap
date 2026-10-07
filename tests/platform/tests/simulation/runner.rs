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
