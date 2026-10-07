//! Aggregate builds and bounded execution of isolated contract jobs.
use super::{
    coverage::{Contract, Layer},
    plan::Job,
    report::{self, Outcome, Report},
};
use crate::process::{Logged, Runner};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{process::Command, task::JoinSet};

pub(super) struct Run<'a> {
    pub root: &'a Path,
    pub cold: Option<&'a Path>,
    pub scratch: &'a Path,
    pub logs: &'a Path,
    pub verbose: bool,
    pub timeout: Duration,
    pub runner: &'a Runner,
}

pub(super) fn command(
    args: &[String],
    root: &Path,
    cold: Option<&Path>,
    scratch: &Path,
) -> Command {
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .current_dir(root)
        .env("TMPDIR", scratch)
        .env("SNAP_BROWSER_ROOT", root)
        .env_remove("SNAP_MASTER_KEY");
    if let Some(target) = cold {
        command
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_BUILD_BUILD_DIR", target);
    }
    command
}

impl Run<'_> {
    pub async fn step(&self, report: &mut Report, name: &str, args: Vec<String>) -> Result<Logged> {
        report.sequence += 1;
        let log = self.logs.join(format!("{:04}", report.sequence));
        println!("  {name} ...");
        if self.verbose {
            println!("    + {}", args.join(" "));
        }
        fs::write(log.with_extension("command"), args.join("\n"))?;
        let start = Instant::now();
        let mut command = command(&args, self.root, self.cold, self.scratch);
        let mut future = std::pin::pin!(self.runner.logged(&mut command, &log, self.verbose, None));
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.tick().await;
        let output = loop {
            tokio::select! {
                result = &mut future => break result?,
                _ = tick.tick() => println!("    {name} · {:.0}s elapsed", start.elapsed().as_secs_f64()),
            }
        };
        report.warnings.extend(report::warnings(&output));
        let cancelled = self.runner.check().is_err();
        let success = output.status.success();
        let status = if cancelled {
            "CANCELLED"
        } else if success {
            "PASS"
        } else {
            "FAIL"
        };
        println!("  {name}  {status}  {:.2}s", start.elapsed().as_secs_f64());
        if !success && !cancelled && !self.verbose {
            report::failure(&output, &log);
        }
        report.outcomes.push(Outcome {
            contract: Contract {
                layer: Layer::Tooling,
                module: "Preflight".into(),
                contract: name.into(),
                configuration: "workspace".into(),
                technique: "static/build",
                storage: None,
                host: None,
                requirement: None,
            },
            status: status.into(),
            passed: 0,
            selected_cases: None,
            duration_ms: start.elapsed().as_millis(),
            log: Some(log.clone()),
        });
        self.runner.check()?;
        output
            .successful()
            .with_context(|| format!("{name} failed"))?;
        Ok(output)
    }

    pub async fn layer(
        &self,
        report: &mut Report,
        layer: Layer,
        jobs: Vec<Job>,
        parallel: u16,
    ) -> Result<()> {
        println!("\n{}", layer.name());
        let total = jobs.len();
        let mut queue: VecDeque<_> = jobs.into();
        let mut active = JoinSet::new();
        let mut running = HashMap::new();
        let mut completed = 0;
        let mut failed = false;
        let mut execution_error = None;
        let terminal = std::io::stdout().is_terminal() && !self.verbose;
        let mut tick = tokio::time::interval(Duration::from_secs(if terminal { 1 } else { 5 }));
        tick.tick().await;
        let start = Instant::now();
        let options = ExecutionOptions {
            verbose: self.verbose,
            timeout: self.timeout,
        };
        loop {
            while active.len() < usize::from(parallel) && !failed && self.runner.check().is_ok() {
                let Some(job) = queue.pop_front() else {
                    break;
                };
                report.sequence += 1;
                let log = self.logs.join(format!("{:04}", report.sequence));
                let contract = job.contract.clone();
                if self.verbose {
                    println!("  Running {} · logs {}", contract.label(), log.display());
                }
                let expected_cases = job.expected_cases;
                let path = log.clone();
                let handle = active.spawn(execute(
                    job,
                    self.runner.clone(),
                    self.root.to_owned(),
                    self.cold.map(Path::to_owned),
                    self.scratch.to_owned(),
                    log,
                    options,
                ));
                running.insert(
                    handle.id(),
                    (contract, path, Instant::now(), expected_cases),
                );
            }
            if active.is_empty() {
                break;
            }
            tokio::select! {
                result = active.join_next_with_id() => {
                    let (id, result) = match result.context("Missing test process")? {
                        Ok((id, result)) => (id, result),
                        Err(error) => (error.id(), Err(error.into())),
                    };
                    let (contract, log, started, selected_cases) = running.remove(&id).context("Missing running contract")?;
                    completed += 1;
                    let (outcome, warnings) = match result {
                        Ok(result) => result,
                        Err(error) => {
                            failed = true;
                            eprintln!("\n{} · {error:#}", contract.label());
                            report.outcomes.push(Outcome { contract, status: if self.runner.check().is_err() { "CANCELLED" } else { "FAIL" }.into(), passed: 0, selected_cases, duration_ms: started.elapsed().as_millis(), log: Some(log) });
                            execution_error = Some(error);
                            continue;
                        }
                    };
                    failed |= outcome.status != "PASS";
                    if self.verbose { println!("  {}  {}  {:.2}s", outcome.contract.label(), outcome.status, outcome.duration_ms as f64 / 1000.0); }
                    report.warnings.extend(warnings);
                    report.outcomes.push(outcome);
                },
                _ = tick.tick() => {
                    let current = running.values().min_by_key(|(_, _, started, _)| *started).map(|(contract, _, _, _)| format!("{} / {} / {}", contract.module, contract.contract, contract.configuration)).unwrap_or_default();
                    if terminal {
                        print!("\r  {completed}/{total} jobs complete · {current}\x1b[K");
                        std::io::stdout().flush()?;
                    } else {
                        println!("  {completed}/{total} jobs complete · {} running · {:.0}s elapsed", active.len(), start.elapsed().as_secs_f64());
                        println!("    Running: {current}");
                    }
                }
            }
        }
        if terminal {
            print!("\r\x1b[K");
        }
        report.outcomes.extend(
            queue
                .into_iter()
                .map(|job| job.not_run("previous failure or cancellation")),
        );
        report.render(layer, self.verbose);
        self.runner.check()?;
        if let Some(error) = execution_error {
            return Err(error);
        }
        ensure!(
            !failed,
            "{} contracts failed; later layers were not run",
            layer.name()
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ExecutionOptions {
    verbose: bool,
    timeout: Duration,
}

async fn execute(
    job: Job,
    runner: Runner,
    root: PathBuf,
    cold: Option<PathBuf>,
    scratch: PathBuf,
    log: PathBuf,
    options: ExecutionOptions,
) -> Result<(Outcome, BTreeSet<String>)> {
    let ExecutionOptions { verbose, timeout } = options;
    fs::write(
        log.with_extension("command"),
        format!("cwd: {}\n{}", job.directory.display(), job.args.join("\n")),
    )?;
    let mut command = command(&job.args, &root, cold.as_deref(), &scratch);
    command
        .current_dir(&job.directory)
        .env("CARGO_MANIFEST_DIR", &job.directory);
    if job.contract.technique == "browser" {
        command.env("SNAP_BROWSER_ARTIFACTS", log.with_extension("artifacts"));
    }
    let start = Instant::now();
    let output = runner
        .logged(&mut command, &log, verbose, Some(timeout))
        .await?;
    let count = report::passing_cases(&output.stdout);
    let cancelled = runner.check().is_err();
    let success = !cancelled
        && !output.timed_out
        && output.status.success()
        && job
            .expected_cases
            .is_none_or(|expected| expected > 0 && count == expected);
    if !success && !cancelled {
        eprintln!("\n{}  FAIL", job.contract.label());
        if !verbose {
            report::failure(&output, &log);
        }
        if output.timed_out {
            eprintln!(
                "Contract job exceeded its {}s execution deadline",
                timeout.as_secs()
            );
        }
        if output.status.success()
            && !output.timed_out
            && let Some(expected) = job.expected_cases
        {
            eprintln!("Expected {expected} contract cases; {count} passed");
        }
    }
    let outcome = Outcome {
        contract: job.contract,
        status: if cancelled {
            "CANCELLED"
        } else if success {
            "PASS"
        } else {
            "FAIL"
        }
        .into(),
        passed: count,
        selected_cases: job.expected_cases,
        duration_ms: start.elapsed().as_millis(),
        log: Some(log),
    };
    Ok((outcome, report::warnings(&output)))
}
