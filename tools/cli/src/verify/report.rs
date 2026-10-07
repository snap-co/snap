//! Coverage summaries and retained evidence, independent of scheduling.
use super::coverage::{Contract, Layer};
use crate::process::Logged;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub(super) struct Outcome {
    #[serde(flatten)]
    pub contract: Contract,
    pub status: String,
    pub passed: usize,
    pub selected_cases: Option<usize>,
    pub duration_ms: u128,
    pub log: Option<PathBuf>,
}

#[derive(Default, Serialize)]
pub(super) struct Report {
    pub selection: BTreeMap<&'static str, String>,
    pub outcomes: Vec<Outcome>,
    pub warnings: BTreeSet<String>,
    pub gaps: Vec<&'static str>,
    #[serde(skip)]
    pub sequence: usize,
}

pub(super) fn passing_cases(output: &[u8]) -> usize {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("test result: ok. ")?
                .split_whitespace()
                .next()?
                .parse::<usize>()
                .ok()
        })
        .sum()
}

pub(super) fn warnings(output: &Logged) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for stream in [&output.stdout, &output.stderr] {
        for line in String::from_utf8_lossy(stream).lines() {
            if line.trim_start().starts_with("warning:") {
                result.insert(line.trim().to_owned());
            }
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && value["reason"] == "compiler-message"
                && value["message"]["level"] == "warning"
                && let Some(message) = value["message"]["rendered"].as_str()
            {
                result.insert(message.trim().to_owned());
            }
        }
    }
    result
}

pub(super) fn failure(output: &Logged, log: &Path) {
    eprintln!("\nFailure details:");
    eprint!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!(
        "Full logs: {} and {}",
        log.with_extension("out").display(),
        log.with_extension("err").display()
    );
}

impl Report {
    pub fn render(&self, layer: Layer, verbose: bool) {
        let mut groups: BTreeMap<(&str, &str), Vec<&Outcome>> = BTreeMap::new();
        for outcome in &self.outcomes {
            if outcome.contract.layer == layer && outcome.contract.module != "Preflight" {
                groups
                    .entry((&outcome.contract.module, &outcome.contract.contract))
                    .or_default()
                    .push(outcome);
            }
        }
        let mut module = "";
        for ((owner, contract), outcomes) in groups {
            if owner != module {
                println!("  {owner}");
                module = owner;
            }
            let passed = outcomes
                .iter()
                .filter(|outcome| outcome.status == "PASS")
                .count();
            let status = if outcomes.iter().any(|outcome| outcome.status == "CANCELLED") {
                "CANCELLED"
            } else if outcomes.iter().any(|outcome| outcome.status == "FAIL") {
                "FAIL"
            } else if outcomes
                .iter()
                .any(|outcome| outcome.status.starts_with("NOT RUN"))
            {
                "INCOMPLETE"
            } else if passed > 0 {
                "PASS"
            } else {
                "NOT SELECTED"
            };
            let tests: usize = outcomes.iter().map(|outcome| outcome.passed).sum();
            let configurations: BTreeSet<_> = outcomes
                .iter()
                .filter(|outcome| outcome.status == "PASS")
                .map(|outcome| &outcome.contract.configuration)
                .collect();
            let techniques: BTreeSet<_> = outcomes
                .iter()
                .map(|outcome| outcome.contract.technique)
                .collect();
            let counts = if techniques.contains("browser") {
                "browser suites".into()
            } else {
                format!("{tests} case{}", if tests == 1 { "" } else { "s" })
            };
            println!(
                "    {contract}  {status} · {counts} · {} setup{} · {}",
                configurations.len(),
                if configurations.len() == 1 { "" } else { "s" },
                techniques.into_iter().collect::<Vec<_>>().join(", ")
            );
            let mut rows: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
            for outcome in outcomes {
                rows.entry(&outcome.contract.configuration)
                    .or_default()
                    .insert(&outcome.status);
            }
            for (configuration, statuses) in rows {
                if verbose
                    || matches!(status, "FAIL" | "CANCELLED" | "INCOMPLETE")
                    || statuses.iter().any(|status| *status != "PASS")
                {
                    println!(
                        "      {configuration}  {}",
                        statuses.into_iter().collect::<Vec<_>>().join(", ")
                    );
                }
            }
        }
    }
}
