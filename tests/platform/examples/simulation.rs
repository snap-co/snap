//! Visible replay of the unchanged cartridge through simulated dependency IO.
#[path = "../support/host.rs"]
mod host;
#[path = "../support/simulation.rs"]
mod setup;
use snap_platform_tests::{journey, simulation::Schedule};
use snap_transport::client::Client;
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut schedule = Schedule::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: simulation [--seed U64] [--jitter-ms U64]\n\nRun the unchanged platform cartridge with simulated Transport and Store.\nNo sockets, files, threads, sleeps or wall-clock time. Print virtual event trace.\nSame seed and configuration replay the same schedule. Workload remains fixed.\nTCP refusal/retirement policy; receipt and submission are separate events.\nClient execution follows wakeups. Output trace times are carrier observations.\nStore IO uses the volatile program interpreter; its synchronous latency blocks\nthe host step. No mid-commit interleaving, SQLite/TLS emulation or crash durability."
                );
                return Ok(());
            }
            "--seed" | "--jitter-ms" => {
                let value: u64 = arguments
                    .next()
                    .ok_or_else(|| io::Error::other(format!("{flag} requires U64")))?
                    .parse()?;
                if flag == "--seed" {
                    schedule.seed = value;
                } else {
                    schedule.jitter_ms = value;
                }
            }
            _ => {
                return Err(
                    io::Error::other(format!("unknown argument {flag}; use --help")).into(),
                );
            }
        }
    }
    println!("Platform: simulated Transport + production host + simulated volatile Store");
    println!("Schedule: {schedule:?}");
    let (mut simulation, timeline, _) = setup::setup(schedule);
    let channel = simulation
        .open()
        .map_err(|error| io::Error::other(format!("open: {error:?}")))?;
    let value = simulation
        .run(async {
            let mut client = Client::new(channel);
            let resumed = client
                .connect("alice", "plumbing-client")
                .await
                .map_err(|error| io::Error::other(format!("connect: {error:?}")))?;
            if resumed {
                return Err(io::Error::other("fresh client unexpectedly resumed"));
            }
            journey::run(&mut client, |_| {})
                .await
                .map_err(|error| io::Error::other(format!("cartridge: {error:?}")))
        })
        .map_err(|error| io::Error::other(format!("scheduler: {error:?}")))??;
    for record in timeline.trace() {
        println!("{:>6} ms {:?}", record.at_ms, record.action);
    }
    println!("Final cartridge read: [{value}, {value}]");
    println!(
        "PASS: all 13 cartridge invocations matched the independent model at {} virtual ms.",
        timeline.now()
    );
    Ok(())
}
