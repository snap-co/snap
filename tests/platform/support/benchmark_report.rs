//! Versioned result files and conservative baseline comparison. Host timing
//! domains never mix; revision changes are expected, input/machine changes aren't.
use super::benchmark::{Config, Host, Result};
use serde::{Deserialize, Serialize};
use snap_platform_tests::{benchmark::Report, cartridge::benchmark::VERSION};
use std::io;

pub const FORMAT_VERSION: u32 = 1;
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Machine {
    pub hostname: String,
    pub os: String,
    pub cpu: String,
    pub logical_cpus: usize,
    pub compiler: String,
    pub build: String,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Metadata {
    pub machine: Machine,
    pub revision: String,
    pub dirty: bool,
    pub dirty_inputs_sha256: String,
    pub executable_sha256: String,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Case {
    pub host: Host,
    pub settings: serde_json::Value,
    pub implementation: serde_json::Value,
    pub response_clock: String,
    pub config: Config,
    pub input_sha256: String,
    pub samples: Vec<Report>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Artifact {
    pub format_version: u32,
    pub workload_version: u32,
    pub metadata: Metadata,
    /// Process high-water RSS across all cases, not a per-case allocation metric.
    pub process_peak_rss_kib: Option<u64>,
    pub cases: Vec<Case>,
}
impl Artifact {
    pub fn new(metadata: Metadata) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            workload_version: VERSION,
            metadata,
            process_peak_rss_kib: None,
            cases: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.format_version != FORMAT_VERSION
            || self.workload_version != VERSION
            || self.cases.is_empty()
        {
            return Err(io::Error::other("unsupported benchmark result/workload version").into());
        }
        for (index, case) in self.cases.iter().enumerate() {
            case.config.validate()?;
            if self.cases[..index]
                .iter()
                .any(|prior| prior.host == case.host && prior.config == case.config)
            {
                return Err(io::Error::other("duplicate benchmark case").into());
            }
            if case.settings != case.host.settings()
                || case.response_clock != case.host.response_clock()
                || case.input_sha256 != snap_platform_tests::runner::hex(&case.config.plan().sha256)
                || case.samples.is_empty()
            {
                return Err(
                    io::Error::other("benchmark configuration/input identity mismatch").into(),
                );
            }
            for sample in &case.samples {
                if [
                    sample.counts.reads,
                    sample.counts.commits,
                    sample.counts.conflicts,
                ]
                .into_iter()
                .any(|count| count > case.config.operations)
                    || sample.counts.completed() != case.config.operations
                    || sample.wall_elapsed_ns == 0
                    || sample.actors.len() != case.config.clients
                    || sample.read_latency.samples != sample.counts.reads
                    || sample.commit_latency.samples != sample.counts.commits
                    || sample.conflict_latency.samples != sample.counts.conflicts
                {
                    return Err(io::Error::other("incomplete benchmark sample").into());
                }
                let mut reads = 0;
                let mut commits = 0;
                let mut conflicts = 0;
                for (actor, counts) in sample.actors.iter().enumerate() {
                    if [counts.reads, counts.commits, counts.conflicts]
                        .into_iter()
                        .any(|count| count > case.config.operations)
                        || counts.completed()
                            != snap_platform_tests::benchmark::actor_operations(
                                case.config.operations,
                                case.config.clients,
                                actor,
                            )
                    {
                        return Err(io::Error::other("benchmark actor count mismatch").into());
                    }
                    reads += counts.reads;
                    commits += counts.commits;
                    conflicts += counts.conflicts;
                }
                if (reads, commits, conflicts)
                    != (
                        sample.counts.reads,
                        sample.counts.commits,
                        sample.counts.conflicts,
                    )
                {
                    return Err(io::Error::other("benchmark totals mismatch").into());
                }
                for latency in [
                    &sample.read_latency,
                    &sample.commit_latency,
                    &sample.conflict_latency,
                ] {
                    match (
                        latency.samples,
                        latency.p50_ns,
                        latency.p95_ns,
                        latency.p99_ns,
                    ) {
                        (0, None, None, None) => {}
                        (count, Some(p50), Some(p95), Some(p99))
                            if count > 0 && p50 <= p95 && p95 <= p99 => {}
                        _ => return Err(io::Error::other("invalid benchmark percentiles").into()),
                    }
                }
            }
        }
        Ok(())
    }
    pub fn baseline_case<'a>(&self, baseline: &'a Artifact, case: &Case) -> Result<&'a Case> {
        baseline.validate()?;
        if self.metadata.machine != baseline.metadata.machine {
            return Err(io::Error::other(
                "baseline machine/compiler/build differs; refusing performance percentages",
            )
            .into());
        }
        baseline
            .cases
            .iter()
            .find(|prior| {
                prior.host == case.host
                    && prior.config == case.config
                    && prior.settings == case.settings
                    && prior.input_sha256 == case.input_sha256
            })
            .ok_or_else(|| {
                io::Error::other(format!(
                    "no compatible baseline for {} / {:?} / {} clients",
                    case.host.name(),
                    case.config.profile,
                    case.config.clients
                ))
                .into()
            })
    }
}

pub fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}
pub fn rates(case: &Case, count: impl Fn(&Report) -> u64) -> Vec<f64> {
    case.samples
        .iter()
        .map(|sample| count(sample) as f64 * 1e9 / sample.wall_elapsed_ns as f64)
        .collect()
}
pub fn success_rate(case: &Case) -> f64 {
    median(rates(case, |sample| sample.counts.successful()))
}
pub fn commit_rate(case: &Case) -> f64 {
    median(rates(case, |sample| sample.counts.commits))
}
pub fn commit_latency_ns(
    case: &Case,
    percentile: impl Fn(&snap_platform_tests::benchmark::Latency) -> Option<u64>,
) -> Option<f64> {
    let values = case
        .samples
        .iter()
        .filter_map(|sample| percentile(&sample.commit_latency))
        .map(|value| value as f64)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| median(values))
}
pub fn delta(current: f64, baseline: f64) -> Option<f64> {
    (baseline > 0.0).then(|| 100.0 * (current / baseline - 1.0))
}
