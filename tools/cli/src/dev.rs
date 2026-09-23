use crate::{build, config::Project, process::Runner};
use anyhow::{Context, Result, ensure};
use nix::{
    sys::signal::{Signal, kill},
    unistd::{Pid, Uid},
};
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

pub async fn run(project: Project, runner: &Runner) -> Result<()> {
    eprintln!(
        "Developing {} ({})",
        project.config.application,
        project.root.display()
    );
    for command in &project.config.prepare.dev {
        runner
            .run(
                Command::new(&command[0])
                    .args(&command[1..])
                    .current_dir(&project.root),
                false,
            )
            .await?;
    }
    let web = match &project.config.web {
        Some(web) => Some(build::web(&project, runner, web).await?),
        None => None,
    };
    let server = &project.config.server;
    let target = match (&server.bin, &server.example) {
        (Some(name), _) => ["--bin", name.as_str()],
        (_, Some(name)) => ["--example", name.as_str()],
        _ => unreachable!("config validates target"),
    };
    let executable = build::cargo(&project, runner, &server.manifest, &target, false).await?;
    let build = match std::env::var("SNAP_BUILD") {
        Ok(value) => value,
        Err(_) => format!(
            "{}-{}",
            project.config.application,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ),
    };
    replace_listener(runner, project.address.port()).await?;
    let mut command = Command::new(executable);
    command
        .current_dir(&project.root)
        .env("SNAP_ADDR", project.address.to_string())
        .env("SNAP_BUILD", build)
        .env("SNAP_APPLICATION", &project.config.application)
        .env(
            "SNAP_ENV",
            std::env::var("SNAP_ENV").unwrap_or_else(|_| "development".into()),
        );
    // Never inherit another app's development assets.
    command.env_remove("SNAP_WEB_DIR");
    if let Some(web) = web {
        command.env("SNAP_WEB_DIR", web);
    }
    runner.run(&mut command, false).await?;
    Ok(())
}

async fn listeners(runner: &Runner, port: u16) -> Result<BTreeSet<i32>> {
    let (status, output) = runner
        .status(
            Command::new("lsof")
                .args(["-nP", "-t", "-a", "-u"])
                .arg(Uid::current().to_string())
                .arg(format!("-iTCP:{port}"))
                .arg("-sTCP:LISTEN"),
            true,
        )
        .await
        .context("snap dev requires lsof for development listener replacement")?;
    ensure!(
        status.success() || status.code() == Some(1),
        "lsof failed: {status}"
    );
    String::from_utf8(output)?
        .lines()
        .map(|line| line.parse().context("Invalid listener PID from lsof"))
        .collect()
}

async fn replace_listener(runner: &Runner, port: u16) -> Result<()> {
    if port == 0 {
        return Ok(());
    }
    let previous = listeners(runner, port).await?;
    if previous.is_empty() {
        return Ok(());
    }
    eprintln!("Replacing listener on port {port}: {previous:?}");
    for signal in [Signal::SIGTERM, Signal::SIGKILL] {
        let current = listeners(runner, port).await?;
        for pid in previous.intersection(&current) {
            let _ = kill(Pid::from_raw(*pid), signal);
        }
        for _ in 0..30 {
            if previous.is_disjoint(&listeners(runner, port).await?) {
                return Ok(());
            }
            runner.pause().await?;
        }
    }
    anyhow::bail!("Listener did not release port {port}")
}
