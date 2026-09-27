use crate::{architecture, cargo, config::Project, process::Runner};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use tokio::process::Command;

pub async fn run(
    project: Project,
    runner: &Runner,
    structure_only: bool,
    workspace: bool,
) -> Result<()> {
    let manifests = if project.config.check.rust.is_empty() {
        vec!["Cargo.toml".into()]
    } else {
        project.config.check.rust.clone()
    };
    if structure_only {
        architecture::check(&project, runner, &manifests, workspace).await?;
        println!("Structural checks passed: {}", project.config.application);
        return Ok(());
    }
    let mut selected = BTreeSet::new();
    for manifest in manifests {
        let (manifest, metadata) = cargo::metadata(&project, runner, &manifest).await?;
        if !selected.insert(manifest.clone()) {
            continue;
        }
        let package = cargo::selected_package(&metadata, &manifest)?;
        let name = package["name"]
            .as_str()
            .context("Missing Cargo package name")?;
        for (task, args) in [
            ("fmt", vec!["--", "--check"]),
            ("clippy", vec!["--all-targets", "--", "-D", "warnings"]),
            ("test", vec!["--all-targets"]),
        ] {
            eprintln!("Checking {name}: cargo {task}");
            let mut command = Command::new("cargo");
            command
                .current_dir(&project.root)
                .arg(task)
                .arg("--manifest-path")
                .arg(&manifest)
                .args(["--package", name])
                .args(args);
            runner.run(&mut command, false).await.with_context(|| {
                format!(
                    "{name}: cargo {task} failed; run it with --manifest-path {}",
                    manifest.display()
                )
            })?;
        }
    }
    for args in &project.config.check.commands {
        eprintln!("Checking {}: {:?}", project.config.application, args);
        let mut command = Command::new(&args[0]);
        command.args(&args[1..]).current_dir(&project.root);
        for key in [
            "SNAP_CHECK_EXECUTABLE",
            "SNAP_CHECK_PACKAGE",
            "SNAP_CHECK_WEB_DIR",
        ] {
            command.env_remove(key);
        }
        runner.run(&mut command, false).await
            .with_context(|| format!("Project check failed: {args:?}. Install the command if it is missing; checks are never skipped."))?;
    }
    runner.check()?;
    println!("Checks passed: {}", project.config.application);
    Ok(())
}
