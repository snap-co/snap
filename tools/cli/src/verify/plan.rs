//! Resolve selected artifacts into contract jobs. Metadata previews and runtime
//! case discovery share ownership rules and configured-guarantee selection.
use super::{
    Args, Gate, Matrix,
    coverage::{self, Contract, Layer},
    execution::{Run, command},
    report::{self, Outcome, Report},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use snap_platform_tests::configuration::{Carrier, Guarantee, Host, Storage};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(super) struct Job {
    pub contract: Contract,
    pub directory: PathBuf,
    pub args: Vec<String>,
    pub expected_cases: Option<usize>,
}

impl Job {
    pub fn not_run(self, reason: &str) -> Outcome {
        Outcome {
            contract: self.contract,
            status: format!("NOT RUN · {reason}"),
            passed: 0,
            selected_cases: self.expected_cases,
            duration_ms: 0,
            log: None,
        }
    }
}

pub(super) struct Plan {
    jobs: Vec<Job>,
}

pub(super) fn cargo_args(task: &str, packages: &[&Value], features: &[String]) -> Vec<String> {
    let mut args = vec!["cargo".into(), task.into()];
    for package in packages {
        args.extend(["-p".into(), package["name"].as_str().unwrap().into()]);
    }
    if !features.is_empty() {
        args.extend(["--features".into(), features.join(",")]);
    }
    args
}

pub(super) fn native_features(packages: &[&Value]) -> Vec<String> {
    let mut features = Vec::new();
    for package in packages {
        let name = package["name"].as_str().unwrap();
        for feature in match name {
            "snap-crypto" => &["passkey"][..],
            "snap-transport" => &["native-server"][..],
            "snap-http" => &["native"][..],
            _ => &[],
        } {
            if package["features"].get(feature).is_some() {
                features.push(format!("{name}/{feature}"));
            }
        }
    }
    features
}

fn needs_browser(gate: Gate) -> bool {
    matches!(gate, Gate::All | Gate::Browser | Gate::Clients)
}

fn selected_packages<'a>(packages: &[&'a Value], gate: Gate) -> Vec<&'a Value> {
    packages
        .iter()
        .copied()
        .filter(|package| {
            let name = package["name"].as_str().unwrap();
            name != "snap-browser-tests"
                && match gate {
                    Gate::Properties => name == "snap-core-properties",
                    Gate::Test | Gate::Io => name != "snap-core-properties",
                    _ => true,
                }
        })
        .collect()
}

fn test_names(output: &[u8]) -> BTreeSet<String> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| line.strip_suffix(": test").map(str::to_owned))
        .collect()
}

async fn native(
    run: &Run<'_>,
    report: &mut Report,
    packages: &[&Value],
    args: &Args,
) -> Result<Vec<Job>> {
    let selected = selected_packages(packages, args.gate);
    ensure!(!selected.is_empty(), "No framework test packages selected");
    let mut build = cargo_args("test", &selected, &native_features(&selected));
    build.extend([
        "--no-run".into(),
        "--message-format=json".into(),
        "--future-incompat-report".into(),
    ]);
    let output = run
        .step(report, "Build shared test executables", build)
        .await?;
    let mut artifacts = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["reason"] == "compiler-artifact"
            && value["profile"]["test"] == true
            && let Some(executable) = value["executable"].as_str()
            && let Some(package) = selected
                .iter()
                .find(|package| package["id"] == value["package_id"])
        {
            artifacts.insert(executable.to_owned(), (*package, value["target"].clone()));
        }
    }
    ensure!(
        !artifacts.is_empty(),
        "Cargo did not report any framework test executables"
    );
    let mut jobs: BTreeMap<(Layer, String, String, String, String, bool), Job> = BTreeMap::new();
    for (executable, (package, target)) in artifacts {
        let directory = Path::new(
            package["manifest_path"]
                .as_str()
                .context("Missing package manifest")?,
        )
        .parent()
        .context("Missing package directory")?
        .to_owned();
        report.sequence += 1;
        let mut listings = Vec::new();
        for ignored in [false, true] {
            let log = run.logs.join(format!(
                "{:04}-list{}",
                report.sequence,
                if ignored { "-ignored" } else { "" }
            ));
            let mut args = vec![executable.clone(), "--list".into()];
            if ignored {
                args.push("--ignored".into());
            }
            let mut command = command(&args, run.root, run.cold, run.scratch);
            command
                .current_dir(&directory)
                .env("CARGO_MANIFEST_DIR", &directory);
            let output = run
                .runner
                .logged(&mut command, &log, false, Some(run.timeout))
                .await?;
            run.runner.check()?;
            if !output.status.success() || output.timed_out {
                report::failure(&output, &log);
            }
            output
                .successful()
                .context("Could not enumerate contract cases")?;
            listings.push(test_names(&output.stdout));
        }
        let ignored = listings.pop().unwrap();
        for name in listings.pop().unwrap() {
            // Parent contracts own subprocess-only entrypoints and their environment.
            if matches!(
                name.rsplit("::").next(),
                Some("fixture_child" | "crash_child")
            ) {
                continue;
            }
            let is_ignored = ignored.contains(&name);
            let mut contract = coverage::describe(package, &target, &name);
            if is_ignored && contract.technique == "contracts" {
                contract.technique = "integration";
            }
            if !contract.selected(args.gate, is_ignored) {
                continue;
            }
            let key = (
                contract.layer,
                contract.module.clone(),
                contract.contract.clone(),
                contract.configuration.clone(),
                executable.clone(),
                is_ignored,
            );
            let job = jobs.entry(key).or_insert_with(|| Job {
                contract,
                directory: directory.clone(),
                args: vec![executable.clone()],
                expected_cases: None,
            });
            job.args.push(name);
        }
    }
    let mut result = Vec::new();
    for (_, mut job) in jobs {
        job.expected_cases = Some(job.args.len() - 1);
        job.args.extend([
            "--exact".into(),
            "--include-ignored".into(),
            "--test-threads=1".into(),
        ]);
        if job.contract.technique == "properties" {
            job.args.push("--nocapture".into());
        }
        if let Some(reason) = job.contract.omission(args.matrix) {
            report.outcomes.push(Outcome {
                contract: job.contract,
                status: reason,
                passed: 0,
                selected_cases: job.expected_cases,
                duration_ms: 0,
                log: None,
            });
        } else {
            result.push(job);
        }
    }
    // Unsupported optional guarantees are results, not fabricated zero-case jobs.
    if let Some(technique) = result
        .iter()
        .find(|job| job.contract.requirement == Some(Guarantee::ProcessCrashDurability))
        .map(|job| job.contract.technique)
    {
        for storage in Storage::ALL {
            if !storage.provides(Guarantee::ProcessCrashDurability) {
                report.outcomes.push(Outcome {
                    contract: Contract {
                        layer: Layer::Interface,
                        module: "Store".into(),
                        contract: "Process-crash durability".into(),
                        configuration: format!("native / {}", storage.name()),
                        technique,
                        storage: Some(storage),
                        host: None,
                        requirement: Some(Guarantee::ProcessCrashDurability),
                    },
                    status: format!(
                        "N/A · does not promise {}",
                        Guarantee::ProcessCrashDurability.name()
                    ),
                    passed: 0,
                    selected_cases: None,
                    duration_ms: 0,
                    log: None,
                });
            }
        }
    }
    ensure!(
        !result.is_empty() || needs_browser(args.gate),
        "No contract cases selected for {:?}",
        args.gate
    );
    Ok(result)
}

impl Plan {
    pub async fn prepare(
        run: &Run<'_>,
        report: &mut Report,
        packages: &[&Value],
        args: &Args,
    ) -> Result<Self> {
        let mut jobs = if matches!(args.gate, Gate::Check | Gate::Browser) {
            Vec::new()
        } else {
            native(run, report, packages, args).await?
        };
        if needs_browser(args.gate) {
            // Compile before execution. Each suite owns its Chromium profile and fixtures.
            let output = run
                .step(
                    report,
                    "Build browser runner",
                    vec![
                        "cargo".into(),
                        "build".into(),
                        "-p".into(),
                        "snap-browser-tests".into(),
                        "--message-format=json".into(),
                    ],
                )
                .await?;
            let executable = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|value| {
                    value["reason"] == "compiler-artifact"
                        && value["target"]["name"] == "snap-browser-tests"
                        && value["executable"].is_string()
                })
                .and_then(|value| value["executable"].as_str().map(str::to_owned))
                .context("Cargo did not report the framework browser executable")?;
            for (suite, module) in [("client", "Browser client"), ("react", "React")] {
                jobs.push(Job {
                    contract: Contract {
                        layer: Layer::Clients,
                        module: module.into(),
                        contract: "Client adapter and UI-kit contracts".into(),
                        configuration: "Chromium / framework fixtures".into(),
                        technique: "browser",
                        storage: None,
                        host: None,
                        requirement: None,
                    },
                    directory: run.root.to_owned(),
                    args: vec![executable.clone(), suite.into()],
                    expected_cases: None,
                });
            }
        }
        Ok(Self { jobs })
    }

    pub async fn execute(self, run: &Run<'_>, report: &mut Report, parallel: u16) -> Result<()> {
        let mut jobs = self.jobs;
        for layer in Layer::ALL {
            let (selected, remaining): (Vec<_>, Vec<_>) = jobs
                .into_iter()
                .partition(|job| job.contract.layer == layer);
            jobs = remaining;
            if !selected.is_empty()
                && let Err(error) = run.layer(report, layer, selected, parallel).await
            {
                report.outcomes.extend(
                    jobs.into_iter()
                        .map(|job| job.not_run("earlier layer failed")),
                );
                return Err(error);
            }
        }
        Ok(())
    }

    /// A no-build target preview. Exact cases and case-level semantic groups are
    /// resolved from the artifacts by prepare, never inferred from source text.
    pub fn preview(packages: &[&Value], args: &Args) {
        println!(
            "\nPreflight · static checks when selecting all/check; shared compilation before native execution"
        );
        let mut groups = BTreeSet::new();
        for package in selected_packages(packages, args.gate) {
            if let Some(targets) = package["targets"].as_array() {
                for target in targets {
                    if target["test"] == false {
                        continue;
                    }
                    let contract = coverage::describe(package, target, "");
                    if contract.selected(args.gate, false) || contract.selected(args.gate, true) {
                        groups.insert((
                            contract.layer,
                            contract.module,
                            target["name"].as_str().unwrap_or("unknown").to_owned(),
                        ));
                    }
                }
            }
        }
        for layer in Layer::ALL {
            let targets: Vec<_> = groups
                .iter()
                .filter(|(selected, _, _)| *selected == layer)
                .collect();
            if targets.is_empty() && !(layer == Layer::Clients && needs_browser(args.gate)) {
                continue;
            }
            println!("\n{}", layer.name());
            for (_, owner, target) in targets {
                println!("  {owner} / {target}");
            }
            if layer == Layer::Clients && needs_browser(args.gate) {
                println!("  Browser client / framework client adapter");
                println!("  React / UI-kit contracts");
            }
        }
        if matches!(args.gate, Gate::All | Gate::Interface | Gate::Test)
            && packages
                .iter()
                .any(|package| package["name"] == "snap-platform-tests")
        {
            println!("\nHost composition matrix · native · client + server SDK");
            for storage in Storage::ALL {
                for carrier in Carrier::ALL {
                    let host = Host { storage, carrier };
                    println!(
                        "  {} / {}  {}",
                        storage.name(),
                        carrier.name(),
                        if matches!(args.matrix, Matrix::Full) || host.fast() {
                            "SELECTED"
                        } else {
                            "NOT SELECTED"
                        }
                    );
                }
            }
        }
        if matches!(args.gate, Gate::All | Gate::Interface | Gate::Io) {
            println!("\nProcess-crash durability requires persistence");
            for storage in Storage::ALL {
                println!(
                    "  {}  {}",
                    storage.name(),
                    if storage.provides(Guarantee::ProcessCrashDurability) {
                        "APPLICABLE"
                    } else {
                        "N/A · no persistence"
                    }
                );
            }
        }
        println!(
            "\nTarget preview only. Cases and ignored-case applicability are enumerated from compiled executables during verification."
        );
    }
}
