//! Seeded generated world and SDK campaign. App policy is in workload::Probe;
//! scheduling/fault policy is in Simulation, and budgets/recording are in runner.
#[path = "../support/host.rs"]
mod host;
#[path = "../support/simulation.rs"]
mod setup;
use snap_platform_tests::{
    runner::{self, Budget, Config, Random, Transcript},
    simulation::Schedule,
    workload::{Probe, World},
};
use snap_transport::client::Client;
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut seed = 42;
    let mut budget = Budget::Operations(10_000);
    let mut selected_budget = false;
    let mut jitter_ms = 20;
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: campaign [--seed U64] [--ops U64 | --time-ms U64] [--jitter-ms U64]\n\nGenerate a valid world and drive the platform cartridge through its real SDK.\nDefaults: seed 42, 10000 operations, jitter 20 ms. One operation is one SDK\ninvocation, including verification reads; world setup is excluded from the budget.\nTime is virtual and starts after setup. A synchronous host callback can overrun\nthe horizon; reported clock and overrun are never clamped. Pending work is not\ndrained on budget completion. Reports command, observation and timed-event SHA-256.\nReplay requires the same code, seed and configuration. No crypto, restart,\nmid-callback interleaving, network loss campaign or generic schema generator yet."
                );
                return Ok(());
            }
            "--seed" | "--ops" | "--time-ms" | "--jitter-ms" => {
                let value: u64 = arguments
                    .next()
                    .ok_or_else(|| io::Error::other(format!("{flag} requires U64")))?
                    .parse()?;
                match flag.as_str() {
                    "--seed" => seed = value,
                    "--jitter-ms" => jitter_ms = value,
                    _ => {
                        if selected_budget {
                            return Err(
                                io::Error::other("select --ops or --time-ms only once").into()
                            );
                        }
                        selected_budget = true;
                        budget = if flag == "--ops" {
                            Budget::Operations(value)
                        } else {
                            Budget::TimeMs(value)
                        };
                    }
                }
            }
            _ => {
                return Err(
                    io::Error::other(format!("unknown argument {flag}; use --help")).into(),
                );
            }
        }
    }
    let config = Config { seed, budget };
    // Host watchdogs are independent of the requested campaign budget. Permit
    // million-operation runs without disabling a runaway event/poll safety limit.
    let schedule = Schedule {
        seed: Random::stream(seed, "schedule").next_u64(),
        jitter_ms,
        max_events: 100_000_000,
        max_polls: 100_000_000,
        max_time_ms: u64::MAX,
        trace_capacity: 128,
        ..Default::default()
    };
    let world = World::generate(seed);
    println!("Campaign v1: {config:?}\nWorld: {world:?}\nHost: {schedule:?}");
    let (mut simulation, timeline, _) = setup::setup(schedule);
    let transcript = Transcript::default();
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<_, io::Error> {
        let channel = simulation
            .open()
            .map_err(|error| io::Error::other(format!("open: {error:?}")))?;
        let mut client = Client::new(transcript.channel(channel));
        simulation
            .run(client.connect("alice", "campaign-client"))
            .map_err(|error| io::Error::other(format!("connect schedule: {error:?}")))?
            .map_err(|error| io::Error::other(format!("connect: {error:?}")))?;
        let mut workload = Probe::new(client);
        simulation
            .run(workload.initialize(&world))
            .map_err(|error| io::Error::other(format!("world setup: {error:?}")))?;
        let report = runner::run(&mut simulation, &mut workload, config)
            .map_err(|error| io::Error::other(format!("campaign: {error:?}")))?;
        // Snapshot before workload Drop queues physical teardown, so fingerprints
        // describe the exact campaign endpoint, not subsequent cleanup work.
        Ok((
            report,
            workload.expected_value(),
            transcript.summary(),
            timeline.events_sha256(),
        ))
    }));
    match outcome {
        Ok(Ok((report, value, streams, events))) => {
            println!(
                "Completed {} of {} started operations; model target {value}",
                report.completed, report.started
            );
            println!(
                "Virtual clock {}..{} ms; overrun {} ms",
                report.start_ms, report.end_ms, report.overrun_ms
            );
            println!("actions      {}", runner::hex(&report.actions_sha256));
            println!(
                "commands     {} ({} sends including setup)",
                runner::hex(&streams.commands_sha256),
                streams.sent
            );
            println!(
                "observations {} ({} receives including setup)",
                runner::hex(&streams.observations_sha256),
                streams.received
            );
            println!("events       {}", runner::hex(&events));
            println!(
                "Recent trace: {} retained, {} discarded",
                timeline.trace().len(),
                timeline.discarded_records()
            );
            Ok(())
        }
        failure => {
            eprintln!("Campaign failed: {config:?}; world={world:?}; host={schedule:?}");
            let streams = transcript.summary();
            eprintln!(
                "At {} virtual ms: {} sends, {} receives",
                timeline.now(),
                streams.sent,
                streams.received
            );
            eprintln!(
                "Commands: {}; observations: {}",
                runner::hex(&streams.commands_sha256),
                runner::hex(&streams.observations_sha256)
            );
            for record in timeline.trace() {
                eprintln!("{:>8} ms {:?}", record.at_ms, record.action);
            }
            match failure {
                Ok(Err(error)) => Err(error.into()),
                Err(panic) => std::panic::resume_unwind(panic),
                _ => unreachable!(),
            }
        }
    }
}
