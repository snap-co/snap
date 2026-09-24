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
    let address = project.address()?;
    let artifacts = build::run(&project, runner, build::Mode::Dev).await?;
    let build = match std::env::var("SNAP_BUILD") {
        Ok(value) => value,
        Err(_) => format!(
            "{}-{}",
            project.config.application,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ),
    };
    let mut command = Command::new(&artifacts.executable);
    command
        .current_dir(&project.root)
        .env("SNAP_ADDR", address.to_string())
        .env("SNAP_BUILD", build)
        .env("SNAP_APPLICATION", &project.config.application)
        .env(
            "SNAP_ENV",
            std::env::var("SNAP_ENV").unwrap_or_else(|_| "development".into()),
        );
    // Never inherit another app's development assets.
    command.env_remove("SNAP_WEB_DIR");
    if let (Some(web), Some(assets)) = (&project.config.web, &artifacts.web) {
        // Only the frontend owns the public address; application HTTP stays private.
        command
            .env("SNAP_ADDR", "127.0.0.1:0")
            .env("SNAP_WEB_DIR", assets);
        let (mut backend, backend_url) = runner.service(&mut command, "listening on ").await?;
        eprintln!("Backend ready at {backend_url}");
        let driver = project.root.join(".snap/dev-web.ts");
        std::fs::write(&driver, include_str!("../../../scripts/dev-web.ts"))?;
        replace_listener(runner, address.port()).await?;
        let (mut frontend, url) = runner
            .service(
                Command::new("bun")
                    .current_dir(&project.root)
                    .arg(&driver)
                    .arg(&project.root)
                    .arg(project.path(&web.package_dir))
                    .arg(project.file(&web.application)?)
                    .arg(project.file(&web.host)?)
                    .arg(project.file(&web.html)?)
                    .arg(assets.join("snap_client_wasm_bg.wasm"))
                    .arg(&backend_url)
                    .arg(address.to_string()),
                "snap-web-ready ",
            )
            .await?;
        eprintln!("listening on {url}");
        let result = tokio::select! {
            result = backend.wait(runner) => result,
            result = frontend.wait(runner) => result,
        };
        let (back, front) = tokio::join!(backend.stop(), frontend.stop());
        result?;
        back?;
        front?;
    } else {
        replace_listener(runner, address.port()).await?;
        runner.run(&mut command, false).await?;
    }
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
