//! Benchmark contracts at measurement and real SDK/host boundaries. These are
//! accounting/portability checks, never noisy performance pass/fail thresholds.
#[path = "../../support/benchmark.rs"]
mod benchmark;
#[path = "../../support/benchmark_report.rs"]
mod report;
use crate::native::{TcpChannel, tls_support};

use snap_platform_tests::{
    benchmark::{self as measurement, Clock, Outcome, Workload},
    cartridge::benchmark::Profile,
};
use std::{cell::Cell, rc::Rc, time::Duration};

#[derive(Clone, Default)]
struct CounterClock(Rc<Cell<u64>>);
impl Clock for CounterClock {
    fn now_ns(&self) -> u64 {
        self.0.get()
    }
}
struct Script {
    response: CounterClock,
    wall: CounterClock,
    step: usize,
    fail: bool,
    offset: u64,
}
impl Workload for Script {
    async fn execute(&mut self) -> Result<Outcome, String> {
        if self.fail {
            return Err("unexpected SDK failure".into());
        }
        self.step += 1;
        self.response
            .0
            .set(self.response.0.get() + self.step as u64 * 10 + self.offset);
        self.wall.0.set(self.wall.0.get() + 7);
        Ok([Outcome::Read, Outcome::Commit, Outcome::Conflict][(self.step - 1) % 3])
    }
}

#[tokio::test]
async fn measurement_counts_outcomes_and_uses_declared_clocks_without_setup_or_sorting() {
    let response = CounterClock::default();
    let wall = CounterClock::default();
    // Prior elapsed time stands for setup. It must not enter either duration.
    response.0.set(500);
    wall.0.set(200);
    let mut actors: Vec<_> = (0..2)
        .map(|actor| Script {
            response: response.clone(),
            wall: wall.clone(),
            step: 0,
            fail: false,
            offset: actor * 5,
        })
        .collect();
    let result = measurement::run(&mut actors, 7, &response, &wall)
        .await
        .unwrap();
    assert_eq!(
        (
            result.counts.reads,
            result.counts.commits,
            result.counts.conflicts
        ),
        (3, 2, 2)
    );
    assert_eq!(
        result
            .actors
            .iter()
            .map(|counts| counts.completed())
            .collect::<Vec<_>>(),
        [4, 3]
    );
    assert_eq!(result.wall_elapsed_ns, 49);
    assert_eq!(result.response_elapsed_ns, 175);
    assert_eq!(
        (
            result.read_latency.p50_ns,
            result.commit_latency.p95_ns,
            result.conflict_latency.p99_ns
        ),
        (Some(15), Some(25), Some(35))
    );
    assert_eq!(result.read_latency.p95_ns, Some(40));
    let empty = measurement::run(&mut actors, 0, &response, &wall)
        .await
        .unwrap();
    assert_eq!(empty.counts.completed(), 0);
    assert_eq!(empty.commit_latency.p99_ns, None);
    actors[0].fail = true;
    assert_eq!(
        measurement::run(&mut actors, 1, &response, &wall)
            .await
            .unwrap_err(),
        "unexpected SDK failure"
    );
}

#[tokio::test]
async fn portable_benchmark_excludes_warmup_and_checks_final_sdk_rows_on_each_host() {
    for host in [
        benchmark::Host::Simulation,
        benchmark::Host::TcpMemory,
        benchmark::Host::TcpSqlite,
    ] {
        for profile in [Profile::Independent, Profile::Shared] {
            let config = benchmark::Config {
                seed: 42,
                profile,
                clients: 3,
                warmup: 5,
                operations: 17,
            };
            let result = benchmark::sample(host, &config, Duration::from_secs(10))
                .await
                .unwrap();
            // Warmup partitions 2/2/1 calls; measured partitions 6/6/5. The last
            // actor starts measurement with a write, so exactly 8 measured reads.
            assert_eq!(result.counts.reads, 8, "{host:?}/{profile:?}");
            assert_eq!(result.counts.commits + result.counts.conflicts, 9);
            assert_eq!(
                result
                    .actors
                    .iter()
                    .map(|counts| counts.completed())
                    .collect::<Vec<_>>(),
                [6, 6, 5]
            );
            assert_eq!(result.commit_latency.samples, result.counts.commits);
            assert!(result.wall_elapsed_ns > 0);
            assert!(result.response_elapsed_ns > 0);
            if profile == Profile::Independent {
                assert_eq!(result.counts.commits, 9);
                assert_eq!(result.counts.conflicts, 0);
            } else if host == benchmark::Host::Simulation {
                assert!(
                    result.counts.conflicts > 0,
                    "shared profile must exercise contention"
                );
            }
        }
    }
}

#[test]
fn baseline_refuses_changed_inputs_hosts_machines_and_incomplete_samples() {
    let config = benchmark::Config {
        seed: 42,
        profile: Profile::Independent,
        clients: 2,
        warmup: 0,
        operations: 8,
    };
    let sample = benchmark::simulated(&config).unwrap();
    let make = || {
        let mut artifact = report::Artifact::new(report::Metadata {
            machine: report::Machine {
                hostname: "test".into(),
                os: "test".into(),
                cpu: "test".into(),
                logical_cpus: 2,
                compiler: "test".into(),
                build: "release".into(),
            },
            revision: "test".into(),
            dirty: false,
            dirty_inputs_sha256: "test".into(),
            executable_sha256: "test".into(),
        });
        artifact.cases.push(report::Case {
            host: benchmark::Host::Simulation,
            settings: benchmark::Host::Simulation.settings(),
            implementation: benchmark::Host::Simulation.implementation(),
            response_clock: "virtual".into(),
            input_sha256: snap_platform_tests::runner::hex(&config.plan().sha256),
            config: config.clone(),
            samples: vec![sample.clone()],
        });
        artifact
    };
    let baseline = make();
    let mut current = make();
    current.metadata.revision = "new-revision".into();
    current.cases[0].implementation = serde_json::json!({"single_actor": false});
    current.cases[0].samples[0].wall_elapsed_ns *= 2;
    let prior = current.baseline_case(&baseline, &current.cases[0]).unwrap();
    assert_eq!(
        report::delta(
            report::success_rate(&current.cases[0]),
            report::success_rate(prior)
        ),
        Some(-50.0)
    );
    assert!(report::commit_rate(prior) > 0.0);
    assert!(report::commit_latency_ns(prior, |latency| latency.p95_ns).unwrap() > 0.0);
    assert_eq!(report::delta(1.0, 0.0), None);
    let encoded = serde_json::to_vec(&baseline).unwrap();
    let mut roundtrip: report::Artifact = serde_json::from_slice(&encoded).unwrap();
    roundtrip.validate().unwrap();
    roundtrip.cases[0].samples[0].counts.commits -= 1;
    assert!(roundtrip.validate().is_err());
    current.metadata.machine.logical_cpus = 4;
    assert!(current.baseline_case(&baseline, &current.cases[0]).is_err());
    current.metadata.machine = baseline.metadata.machine.clone();
    current.cases[0].config.seed = 9;
    current.cases[0].input_sha256 =
        snap_platform_tests::runner::hex(&current.cases[0].config.plan().sha256);
    assert!(current.baseline_case(&baseline, &current.cases[0]).is_err());
    current = make();
    current.cases[0].host = benchmark::Host::TcpMemory;
    current.cases[0].settings = benchmark::Host::TcpMemory.settings();
    current.cases[0].response_clock = "wall".into();
    assert!(current.baseline_case(&baseline, &current.cases[0]).is_err());
}
