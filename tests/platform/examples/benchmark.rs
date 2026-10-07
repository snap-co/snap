//! One portable benchmark workload, host-selected assembly and explicit clocks.
#[path = "../support/benchmark.rs"]
mod benchmark;
#[path = "../support/benchmark_report.rs"]
mod report;
#[path = "../support/tcp_channel.rs"]
mod tcp_channel;
#[path = "../../../crates/transport/tests/support/mod.rs"]
mod tls_support;
use tcp_channel::TcpChannel;

use benchmark::{Config, Host, Result};
use report::{Artifact, Case, Machine, Metadata};
use sha2::{Digest, Sha256};
use snap_platform_tests::{cartridge::benchmark::Profile, runner::hex};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

struct Options {
    hosts: Vec<Host>,
    profiles: Vec<Profile>,
    clients: Vec<usize>,
    seed: u64,
    operations: u64,
    warmup: u64,
    samples: usize,
    timeout: Duration,
    output: Option<PathBuf>,
    baseline: Option<PathBuf>,
}
fn parse() -> Result<Option<Options>> {
    let mut options = Options {
        hosts: vec![Host::Simulation],
        profiles: vec![Profile::Independent],
        clients: vec![1, 2, 8, 32],
        seed: 42,
        operations: 1000,
        warmup: 100,
        samples: 5,
        timeout: Duration::from_secs(120),
        output: None,
        baseline: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        if flag == "--help" || flag == "-h" {
            println!(
                "Usage: benchmark [--host simulation|tcp-memory|tcp-sqlite|all]\n  [--profile independent|shared|all] [--clients 1,2,8,32] [--seed U64]\n  [--ops 1..1000000] [--warmup 0..1000000] [--samples 1..100]\n  [--timeout-secs U64] [--output FILE] [--baseline FILE]\n\nDefaults: simulation, independent, clients 1,2,8,32, seed 42, 1000 measured\nSDK invocations TOTAL per sample, 100 warmup invocations, five fresh samples.\nOne outstanding call per client; fixed per-client input ownership, no think\ntime, retries or faults. Actors alternate paired-row reads and guarded writes.\nNative adapters use production TCP/TLS and dispatch with Memory or file SQLite.\nSetup, warmup, final SDK verification and teardown are outside measurement.\nSimulation success/sec is wall-clock simulator efficiency, NOT server capacity.\nSimulation response latency is VIRTUAL, with fixed modeled dependency delays.\nNative response latency is WALL time. Counts distinguish reads/commits/conflicts;\nunknown outcomes and invariant failures fail the run. Exact latency retention is\nO(ops). Simulation still pays for timed-event SHA-256, not the history oracle.\nClosed-loop load has a draining tail, not a fixed arrival rate or overload test.\nNative timeout covers connection, warmup, measurement and verification, not\nsynchronous setup; simulation uses event/poll watchdogs, not a wall deadline.\n--baseline requires matching machine/compiler/build, workload/configuration and\ninput fingerprint. Samples can differ; revisions/completion orders can change.\nPerformance changes are informational, never automatic pass/fail thresholds.\nResult files are created new; existing files are never overwritten."
            );
            return Ok(None);
        }
        let value = arguments
            .next()
            .ok_or_else(|| io::Error::other(format!("{flag} requires a value")))?;
        match flag.as_str() {
            "--host" => {
                options.hosts = match value.as_str() {
                    "simulation" => vec![Host::Simulation],
                    "tcp-memory" => vec![Host::TcpMemory],
                    "tcp-sqlite" => vec![Host::TcpSqlite],
                    "all" => vec![Host::Simulation, Host::TcpMemory, Host::TcpSqlite],
                    _ => return Err(io::Error::other("unknown host; use --help").into()),
                }
            }
            "--profile" => {
                options.profiles = match value.as_str() {
                    "independent" => vec![Profile::Independent],
                    "shared" => vec![Profile::Shared],
                    "all" => vec![Profile::Independent, Profile::Shared],
                    _ => {
                        return Err(io::Error::other("unknown workload profile; use --help").into());
                    }
                }
            }
            "--clients" => {
                options.clients = value
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<_, _>>()?
            }
            "--seed" => options.seed = value.parse()?,
            "--ops" => options.operations = value.parse()?,
            "--warmup" => options.warmup = value.parse()?,
            "--samples" => options.samples = value.parse()?,
            "--timeout-secs" => options.timeout = Duration::from_secs(value.parse()?),
            "--output" => options.output = Some(value.into()),
            "--baseline" => options.baseline = Some(value.into()),
            _ => return Err(io::Error::other(format!("unknown flag {flag}; use --help")).into()),
        }
    }
    if !(1..=100).contains(&options.samples) || options.timeout.is_zero() {
        return Err(io::Error::other("samples must be 1..100 and timeout must be positive").into());
    }
    for (index, &clients) in options.clients.iter().enumerate() {
        if options.clients[..index].contains(&clients) {
            return Err(io::Error::other("duplicate client count").into());
        }
        Config {
            seed: options.seed,
            profile: options.profiles[0],
            clients,
            warmup: options.warmup,
            operations: options.operations,
        }
        .validate()?;
    }
    Ok(Some(options))
}
fn command(root: &Path, name: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(name).args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(output.stdout)
}
fn text(root: &Path, name: &str, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(command(root, name, args)?)?.trim().into())
}
fn metadata() -> Result<Metadata> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu = cpuinfo
        .lines()
        .find_map(|line| {
            line.strip_prefix("model name")
                .and_then(|value| value.split_once(':'))
                .map(|(_, value)| value.trim().to_owned())
        })
        .unwrap_or_else(|| std::env::consts::ARCH.into());
    let mut dirty_hash = Sha256::new();
    dirty_hash.update(b"snap-benchmark-checkout-v1");
    dirty_hash.update(command(&root, "git", &["diff", "HEAD", "--binary"])?);
    let untracked = command(
        &root,
        "git",
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    for path in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let name = std::str::from_utf8(path)?;
        dirty_hash.update((path.len() as u64).to_be_bytes());
        dirty_hash.update(path);
        let bytes = std::fs::read(root.join(name))?;
        dirty_hash.update((bytes.len() as u64).to_be_bytes());
        dirty_hash.update(bytes);
    }
    let executable_sha256 = hex(&Sha256::digest(std::fs::read(std::env::current_exe()?)?).into());
    Ok(Metadata {
        machine: Machine {
            hostname: text(&root, "uname", &["-n"])?,
            os: text(&root, "uname", &["-smr"])?,
            cpu,
            logical_cpus: std::thread::available_parallelism()?.get(),
            compiler: text(&root, "rustc", &["-Vv"])?,
            build: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
        },
        revision: text(&root, "git", &["rev-parse", "HEAD"])?,
        dirty: !command(&root, "git", &["status", "--porcelain"])?.is_empty(),
        dirty_inputs_sha256: hex(&dirty_hash.finalize().into()),
        executable_sha256,
    })
}
fn latency(
    case: &Case,
    percentile: impl Fn(&snap_platform_tests::benchmark::Latency) -> Option<u64>,
) -> String {
    report::commit_latency_ns(case, percentile)
        .map_or_else(|| "n/a".into(), |ns| format!("{:.3}", ns / 1e6))
}
fn print_case(case: &Case, baseline: Option<&Case>) {
    let rates = report::rates(case, |sample| sample.counts.successful());
    let min = rates.iter().copied().reduce(f64::min).unwrap();
    let max = rates.iter().copied().reduce(f64::max).unwrap();
    let conflicts = report::median(
        case.samples
            .iter()
            .map(|sample| 100.0 * sample.counts.conflicts as f64 / sample.counts.completed() as f64)
            .collect(),
    );
    println!(
        "{} / {:?} / {} clients [{} response latency]",
        case.host.name(),
        case.config.profile,
        case.config.clients,
        case.response_clock
    );
    println!(
        "  completed/s {:.1}; success/s {:.1} [{min:.1}..{max:.1}]; commits/s {:.1}; conflicts {conflicts:.1}%",
        report::median(report::rates(case, |sample| sample.counts.completed())),
        report::success_rate(case),
        report::commit_rate(case)
    );
    println!(
        "  commit p50/p95/p99 ms {}/{}/{}; medians across {} fresh samples",
        latency(case, |l| l.p50_ns),
        latency(case, |l| l.p95_ns),
        latency(case, |l| l.p99_ns),
        case.samples.len()
    );
    if let Some(prior) = baseline {
        let change = |current, previous| {
            report::delta(current, previous)
                .map_or_else(|| "n/a".into(), |value| format!("{value:+.1}%"))
        };
        println!(
            "  vs baseline: success/s {}; commits/s {}",
            change(report::success_rate(case), report::success_rate(prior)),
            change(report::commit_rate(case), report::commit_rate(prior))
        );
        let latency_change =
            |percentile: fn(&snap_platform_tests::benchmark::Latency) -> Option<u64>| {
                report::commit_latency_ns(case, percentile)
                    .zip(report::commit_latency_ns(prior, percentile))
                    .and_then(|(current, baseline)| report::delta(current, baseline))
                    .map_or_else(|| "n/a".into(), |change| format!("{change:+.1}%"))
            };
        println!(
            "  vs baseline: commit p95 {}; p99 {} [lower is better]",
            latency_change(|l| l.p95_ns),
            latency_change(|l| l.p99_ns)
        );
        if case.implementation != prior.implementation {
            println!(
                "  implementation changed: {} -> {}",
                prior.implementation, case.implementation
            );
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let Some(options) = parse()? else {
        return Ok(());
    };
    if cfg!(debug_assertions) {
        return Err(io::Error::other("benchmark requires --release").into());
    }
    if let Some(path) = &options.output
        && std::fs::symlink_metadata(path).is_ok()
    {
        return Err(
            io::Error::new(io::ErrorKind::AlreadyExists, "result file already exists").into(),
        );
    }
    let baseline = options
        .baseline
        .as_ref()
        .map(|path| -> Result<Artifact> {
            let artifact: Artifact = serde_json::from_reader(std::fs::File::open(path)?)?;
            artifact.validate()?;
            Ok(artifact)
        })
        .transpose()?;
    let mut artifact = Artifact::new(metadata()?);
    println!(
        "Benchmark v1, workload v1; revision {}; dirty {}",
        artifact.metadata.revision, artifact.metadata.dirty
    );
    println!(
        "{}; {}; {} CPUs",
        artifact.metadata.machine.cpu,
        artifact.metadata.machine.os,
        artifact.metadata.machine.logical_cpus
    );
    println!(
        "Alternating reads/guarded writes, one outstanding call/client, no think time or faults.\nSimulation wall throughput measures simulator efficiency; virtual latency is not native performance."
    );
    println!(
        "Seed {}; {} measured SDK invocations/sample; {} warmup; {} samples",
        options.seed, options.operations, options.warmup, options.samples
    );
    println!(
        "Independent uses one record pair/client; shared uses one pair total. Both declare whole-table data."
    );
    for &host in &options.hosts {
        println!("Host settings: {} {}", host.name(), host.settings());
        println!("Implementation: {}", host.implementation());
        for &profile in &options.profiles {
            for &clients in &options.clients {
                let config = Config {
                    seed: options.seed,
                    profile,
                    clients,
                    warmup: options.warmup,
                    operations: options.operations,
                };
                let mut case = Case {
                    host,
                    settings: host.settings(),
                    implementation: host.implementation(),
                    response_clock: host.response_clock().into(),
                    input_sha256: hex(&config.plan().sha256),
                    config,
                    samples: Vec::new(),
                };
                // Reject incompatible comparisons before spending time on samples.
                let prior = baseline
                    .as_ref()
                    .map(|baseline| artifact.baseline_case(baseline, &case))
                    .transpose()?;
                for _ in 0..options.samples {
                    case.samples
                        .push(benchmark::sample(host, &case.config, options.timeout).await?);
                }
                print_case(&case, prior);
                artifact.cases.push(case);
            }
        }
    }
    artifact.process_peak_rss_kib =
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmHWM:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|value| value.parse().ok())
            });
    artifact.validate()?;
    if let Some(path) = &options.output {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        serde_json::to_writer_pretty(file, &artifact)?;
        println!("Results: {}", path.display());
    }
    Ok(())
}
