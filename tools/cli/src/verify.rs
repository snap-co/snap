//! Framework verification coordinates planning, execution and semantic reporting.
mod coverage;
mod execution;
mod help;
mod plan;
mod report;

use crate::process::Runner;
use anyhow::{Context, Result, ensure};
use clap::{Args as ClapArgs, ValueEnum};
use execution::Run;
use plan::{Plan, cargo_args, native_features};
use report::Report;
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Gate {
    /// Static checks, all implemented native contracts and properties, browser suites, and doctests.
    All,
    /// Interface contracts: Store, Transport, Crypto, HTTP and native host composition, including properties and ignored cases.
    Interface,
    /// Core contracts: Document, Access, Identity and OIDC, including properties and ignored cases.
    Core,
    /// Client adapters: native adapter contracts plus Chromium browser-client and React suites.
    Clients,
    /// Tooling contracts: CLI and deployment configuration, including ignored cases. Does not run static checks.
    Tooling,
    /// Static checks only: formatting, Clippy, dependency direction, portable Wasm compilation, TypeScript and dependency policy.
    Check,
    /// Non-ignored native tests and doctests across layers; excludes properties and browser suites.
    Test,
    /// Generated property tests across their owning layers; excludes fixed examples and browser suites.
    Properties,
    /// Explicitly ignored native integration contracts, including real-IO and cache-reuse checks; excludes properties.
    Io,
    /// Chromium browser-client and React suites only.
    Browser,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Matrix {
    /// Representative native setups. Keeps applicable crash-durability and adapter-specific checks.
    Fast,
    /// Every implemented applicable configuration, not every intended contract/configuration combination.
    Full,
}

#[derive(ClapArgs)]
#[command(
    override_usage = "./bin/test [OPTIONS] [GATE]\n       snap verify-framework [OPTIONS] [GATE]",
    after_help = help::details()
)]
pub struct Args {
    #[arg(value_enum, default_value = "all")]
    gate: Gate,
    /// Framework checkout to verify. bin/test sets this to its own checkout; use the direct CLI to select another root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Preview target ownership and host selections without building. Exact cases are discovered at runtime.
    #[arg(long)]
    list: bool,
    /// Representative host setups, or every implemented combination of the same contracts.
    #[arg(long, value_enum, default_value = "fast")]
    matrix: Matrix,
    /// Maximum independent test processes, 1..=64. Cargo compilation remains aggregated and sequential.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
    jobs: u16,
    /// Stream raw command output and show individual configuration results.
    #[arg(long)]
    verbose: bool,
    /// Maximum execution time per contract job, in seconds, at least 1. Compilation has no test deadline.
    #[arg(long, default_value_t = 1200, value_parser = clap::value_parser!(u32).range(1..))]
    timeout: u32,
    /// Retain fresh build artifacts after successful verification. Requires --cold.
    #[arg(long, requires = "cold")]
    keep: bool,
    /// Use fresh disposable artifacts, without changing the normal Cargo cache.
    #[arg(long)]
    cold: bool,
}

async fn verify(
    run: &Run<'_>,
    report: &mut Report,
    packages: &[&Value],
    args: &Args,
) -> Result<()> {
    println!("\nPreflight");
    if matches!(args.gate, Gate::All | Gate::Check) {
        let mut format = cargo_args("fmt", packages, &[]);
        format.extend(["--".into(), "--check".into()]);
        run.step(report, "Formatting", format).await?;
        for (name, features) in [
            ("Rust linting · default features", Vec::new()),
            (
                "Rust linting · native HTTP, crypto and transport",
                native_features(packages),
            ),
        ] {
            let mut clippy = cargo_args("clippy", packages, &features);
            clippy.extend([
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ]);
            run.step(report, name, clippy).await?;
        }
        run.step(
            report,
            "Dependency direction and portable Wasm compilation",
            vec![
                std::env::current_exe()?.to_string_lossy().into_owned(),
                "check".into(),
                run.root.to_string_lossy().into_owned(),
                "--framework".into(),
                "--structure-only".into(),
            ],
        )
        .await?;
        run.step(
            report,
            "TypeScript",
            vec![
                "bun".into(),
                run.root
                    .join("node_modules/typescript/bin/tsc")
                    .to_string_lossy()
                    .into_owned(),
                "--project".into(),
                run.root
                    .join("kits/tsconfig.json")
                    .to_string_lossy()
                    .into_owned(),
                "--noEmit".into(),
            ],
        )
        .await?;
        run.step(
            report,
            "Dependency policy · unused dependencies, bans and sources",
            vec![
                std::env::current_exe()?.to_string_lossy().into_owned(),
                "check-deps".into(),
                run.root.to_string_lossy().into_owned(),
                "--framework".into(),
            ],
        )
        .await?;
    }
    Plan::prepare(run, report, packages, args)
        .await?
        .execute(run, report, args.jobs)
        .await?;
    if matches!(args.gate, Gate::All | Gate::Test) {
        let selected: Vec<_> = packages
            .iter()
            .copied()
            .filter(|package| {
                !matches!(
                    package["name"].as_str(),
                    Some("snap-core-properties" | "snap-browser-tests")
                )
            })
            .collect();
        let mut docs = cargo_args("test", &selected, &native_features(&selected));
        docs.push("--doc".into());
        run.step(report, "Documentation examples · framework packages", docs)
            .await?;
    }
    Ok(())
}

pub async fn run(args: Args, runner: &Runner) -> Result<()> {
    let root = args.root.canonicalize()?;
    let output = runner
        .run(
            Command::new("cargo").current_dir(&root).args([
                "metadata",
                "--format-version=1",
                "--no-deps",
            ]),
            true,
        )
        .await?;
    runner.check()?;
    let metadata: Value = serde_json::from_slice(&output)?;
    let packages = crate::cargo::workspace_packages(&metadata, true)?;
    ensure!(
        !packages.is_empty(),
        "No framework workspace packages selected"
    );
    println!(
        "Framework verification · {} packages · {:?} matrix · {} parallel jobs",
        packages.len(),
        args.matrix,
        args.jobs
    );
    if args.list {
        Plan::preview(&packages, &args);
        return Ok(());
    }
    let scratch = PathBuf::from(std::env::var_os("HOME").context("HOME is required")?)
        .join(".cache/coding-agents");
    fs::create_dir_all(&scratch)?;
    let cold_root = if args.cold {
        let root = tempfile::Builder::new()
            .prefix("snap-framework-cold-")
            .tempdir_in(&scratch)?
            .keep();
        println!("Cold build artifacts: {}", root.display());
        Some(root)
    } else {
        None
    };
    let cold = cold_root.as_ref().map(|root| root.join("target"));
    let target = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .context("Missing Cargo target directory")?,
    );
    let log_root = target.join("verification");
    fs::create_dir_all(&log_root)?;
    let logs = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&log_root)?
        .keep();
    println!(
        "Cargo cache: {}",
        cold.as_ref().unwrap_or(&target).display()
    );
    println!("Logs: {}", logs.display());
    let mut report = Report::default();
    report.selection.extend([
        ("gate", format!("{:?}", args.gate)),
        ("matrix", format!("{:?}", args.matrix)),
        ("parallel_jobs", args.jobs.to_string()),
        ("job_timeout_seconds", args.timeout.to_string()),
        (
            "cargo_target",
            cold.as_ref().unwrap_or(&target).display().to_string(),
        ),
    ]);
    let start = Instant::now();
    let run = Run {
        root: &root,
        cold: cold.as_deref(),
        scratch: &scratch,
        logs: &logs,
        verbose: args.verbose,
        timeout: Duration::from_secs(u64::from(args.timeout)),
        runner,
    };
    let result = verify(&run, &mut report, &packages, &args)
        .await
        .and_then(|()| runner.check());
    if matches!(args.gate, Gate::All | Gate::Interface) {
        report.gaps.extend(["Shared Wasm platform conformance is not implemented", "Browser client-carrier conformance and client-side durable recovery are not implemented"]);
    }
    if matches!(args.gate, Gate::All | Gate::Core) {
        report.gaps.push("Core host variation currently covers Document manifest holdings; other core contracts retain their existing setups");
    }
    println!(
        "\n{} · {:.2}s · {} distinct warnings",
        if result.is_ok() {
            "Verification passed"
        } else {
            "Verification incomplete"
        },
        start.elapsed().as_secs_f64(),
        report.warnings.len()
    );
    for warning in &report.warnings {
        println!("  {warning}");
    }
    for gap in &report.gaps {
        println!("  Coverage gap: {gap}");
    }
    println!("Full logs and coverage results: {}", logs.display());
    fs::write(
        logs.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    if result.is_ok()
        && !args.keep
        && let Some(root) = &cold_root
    {
        fs::remove_dir_all(root)?;
    }
    if result.is_err()
        && let Some(root) = cold_root
    {
        eprintln!("Retained cold build artifacts: {}", root.display());
    }
    result
}
