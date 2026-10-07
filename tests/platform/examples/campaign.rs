//! Seeded generated world and SDK campaign. App policy is in workload::Probe;
//! scheduling/fault policy is in Simulation, and budgets/recording are in runner.
#[path = "../support/campaign.rs"]
mod campaign_setup;
#[path = "../support/host.rs"]
mod host;
#[path = "../support/simulation.rs"]
mod setup;
use snap_platform_tests::{
    runner::{self, Budget, Config, Random, TranscriptSummary},
    simulation::Schedule,
    workload::World,
};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut seed = 42;
    let mut budget = Budget::Operations(10_000);
    let mut selected_budget = false;
    let mut jitter_ms = 20;
    let mut clients = 2usize;
    let mut faults_enabled = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: campaign [--seed U64] [--clients 1..127] [--ops U64 | --time-ms U64] [--jitter-ms U64] [--faults]\n\nDefaults: seed 42, two SDK clients, 10000 operations, jitter 20 ms.\nClients overlap calls in bounded contention/verification rounds against one\nsingle-actor production server. Each client uses seeded virtual think time.\n--ops counts SDK invocations across ALL clients, including verification reads.\nWorld setup and periodic server checks are excluded from the budget.\nThe server queues a paired-row check every 1000 virtual ms; --faults arms\nconfirmed commit rejection every 97 virtual ms. No automatic mutation retries.\nTime starts after setup. Synchronous callbacks can overrun the horizon.\nPending work is not drained and partial rounds are not fully model-checked.\nReports actor-indexed commands/observations, completion order and timed-event\nSHA-256. Replay requires the same code, seed, client count and configuration.\nNo crypto, restart, mid-callback interleaving or network loss campaign yet."
                );
                return Ok(());
            }
            "--faults" => faults_enabled = true,
            "--seed" | "--ops" | "--time-ms" | "--jitter-ms" | "--clients" => {
                let value: u64 = arguments
                    .next()
                    .ok_or_else(|| io::Error::other(format!("{flag} requires U64")))?
                    .parse()?;
                match flag.as_str() {
                    "--seed" => seed = value,
                    "--jitter-ms" => jitter_ms = value,
                    "--clients" => {
                        clients = usize::try_from(value)?;
                        if !(1..=127).contains(&clients) {
                            return Err(io::Error::other("--clients must be 1..=127").into());
                        }
                    }
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
    println!(
        "Campaign v2: {config:?}; clients={clients}; faults={faults_enabled}\nWorld: {world:?}\nHost: {schedule:?}"
    );
    let mut campaign = campaign_setup::assemble(schedule, &world, clients, faults_enabled)?;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        runner::run_many(&mut campaign.simulation, &mut campaign.actors, config)
    }));
    let timeline = &campaign.timeline;
    let streams = TranscriptSummary::combine(
        &campaign
            .transcripts
            .iter()
            .map(|stream| stream.summary())
            .collect::<Vec<_>>(),
    );
    match outcome {
        Ok(Ok(group)) => {
            let report = &group.campaign;
            let value = campaign.actors[0].expected_value();
            println!(
                "Completed {} of {} started operations; last checked model {value}",
                report.completed, report.started
            );
            println!(
                "Virtual clock {}..{} ms; overrun {} ms",
                report.start_ms, report.end_ms, report.overrun_ms
            );
            println!("actions      {}", runner::hex(&report.actions_sha256));
            println!("completions  {}", runner::hex(&group.completion_sha256));
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
            println!("events       {}", runner::hex(&timeline.events_sha256()));
            println!(
                "Completed rounds: {}; max reserved in flight: {}",
                group.rounds, group.max_in_flight
            );
            println!(
                "Per-client completions: {:?}",
                group
                    .actors
                    .iter()
                    .map(|actor| actor.completed)
                    .collect::<Vec<_>>()
            );
            println!(
                "Server checks: {}; fault injections: {}",
                campaign.server_checks.get(),
                campaign.fault_injections.get()
            );
            println!(
                "Recent trace: {} retained, {} discarded",
                timeline.trace().len(),
                timeline.discarded_records()
            );
            Ok(())
        }
        failure => {
            eprintln!(
                "Campaign failed: {config:?}; clients={clients}; faults={faults_enabled}; world={world:?}; host={schedule:?}"
            );
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
                Ok(Err(error)) => Err(io::Error::other(format!("campaign: {error:?}")).into()),
                Err(panic) => std::panic::resume_unwind(panic),
                _ => unreachable!(),
            }
        }
    }
}
