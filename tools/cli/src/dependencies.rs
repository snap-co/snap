//! Dependency checks use Cargo's active workspace membership, never a filesystem
//! scan. Tools must match mise's pins; this command never installs or upgrades them.
use crate::process::Runner;
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde_json::Value;
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Args)]
pub struct Options {
    /// Start Cargo workspace discovery here
    pub directory: Option<PathBuf>,
    /// Also consult the advisory database
    #[arg(long)]
    pub audit: bool,
    /// Limit unused-dependency checks to framework packages; deny policy is workspace-wide
    #[arg(long)]
    pub framework: bool,
}

pub async fn run(options: Options, runner: &Runner) -> Result<()> {
    let directory = options
        .directory
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&directory)
        .args(["metadata", "--no-deps", "--format-version", "1"]);
    let metadata: Value = serde_json::from_slice(&runner.run(&mut command, true).await?)?;
    let root = PathBuf::from(
        metadata["workspace_root"]
            .as_str()
            .context("Missing workspace root")?,
    );
    let pins: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("mise.toml"))?)?;
    let tools = pins
        .get("tools")
        .and_then(toml::Value::as_table)
        .context("Missing tools table in mise.toml")?;
    for tool in ["machete", "deny"] {
        let expected = tools
            .get(&format!("cargo:cargo-{tool}"))
            .and_then(toml::Value::as_str)
            .context("Missing pinned dependency tool version in mise.toml")?;
        let mut command = Command::new("cargo");
        command.current_dir(&root).args([tool, "--version"]);
        let bytes = runner
            .run(&mut command, true)
            .await
            .context("Install pinned dependency tools with mise install")?;
        let actual = std::str::from_utf8(&bytes)?.trim();
        ensure!(
            actual == expected || actual == format!("cargo-{tool} {expected}"),
            "Expected cargo-{tool} {expected}, got {actual}; run mise install and use mise exec"
        );
    }
    let packages = crate::cargo::workspace_packages(&metadata, options.framework)?;
    ensure!(!packages.is_empty(), "No workspace packages selected");
    let mut machete = Command::new("cargo");
    machete.current_dir(&root).arg("machete");
    for package in packages {
        machete.arg(
            package["manifest_path"]
                .as_str()
                .context("Missing package manifest")?,
        );
    }
    runner.run(&mut machete, false).await?;
    let mut deny = Command::new("cargo");
    deny.current_dir(&root)
        .args(["deny", "check", "bans", "sources"]);
    runner.run(&mut deny, false).await?;
    if options.audit {
        let mut deny = Command::new("cargo");
        deny.current_dir(&root)
            .args(["deny", "check", "advisories"]);
        runner.run(&mut deny, false).await?;
    }
    Ok(())
}
