//! Visible native cartridge runner. The same setup is exercised by Cargo tests.
#[path = "../support/tcp_sqlite.rs"]
mod setup;

use snap_platform_tests::journey::Observation;
use snap_transport::client::Pump;
use std::{io, path::PathBuf};

#[tokio::main(flavor = "current_thread")]
async fn main() -> setup::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let database = match arguments.next() {
        None => {
            let directory = tempfile::Builder::new()
                .prefix("snap-plumbing-")
                .tempdir()?
                .keep();
            directory.join("cartridge.sqlite")
        }
        Some(flag) if flag == "--help" || flag == "-h" => {
            println!(
                "Usage: plumbing [--database NEW_PATH]\n\nRun the platform cartridge through native TCP/TLS and file-backed SQLite.\nWithout --database, retain the database in a fresh temporary directory.\nExisting databases and SQLite companion paths are rejected, including links.\nUse a directory not modified concurrently.\nChecks commits, stale guards, rollback, invalid output, caught MISS, and\nreads after each change; then stops the server and reopens SQLite.\nThis is real IO, not deterministic simulation or power-loss testing."
            );
            return Ok(());
        }
        Some(flag) if flag == "--database" => {
            let path = arguments
                .next()
                .ok_or_else(|| io::Error::other("--database requires a new file path"))?;
            PathBuf::from(path)
        }
        Some(flag) => {
            return Err(io::Error::other(format!("unknown argument {flag:?}; use --help")).into());
        }
    };
    if arguments.next().is_some() {
        return Err(io::Error::other("unexpected argument; use --help").into());
    }
    println!("Platform: native client + TCP/TLS + native server + SQLite");
    println!("Database: {}", database.display());
    let mut calls = 0;
    let rows = setup::run(&database, |observation| match observation {
        Observation::Sent {
            operation,
            id,
            input,
        } => {
            calls += 1;
            println!("-> #{id} {operation} {input}");
        }
        Observation::Received { observation, .. } => match observation {
            Pump::Accepted { id } => println!("<- #{id} accepted"),
            Pump::Completed {
                id,
                outcome: Ok(value),
            } => println!("<- #{id} completed {value}"),
            Pump::Completed {
                id,
                outcome: Err(error),
            } => println!("<- #{id} failed {error:?}"),
            other => println!("<- {other:?}"),
        },
    })
    .await?;
    println!("Reopened SQLite: left={}, right={}", rows[0], rows[1]);
    println!("PASS: {calls} invocations matched the independent model; database retained.");
    Ok(())
}
