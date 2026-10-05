//! Framework verification in a source copy with no application members or files.
use crate::process::Runner;
use anyhow::{Context, Result, ensure};
use clap::{Args as ClapArgs, ValueEnum};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Gate {
    Check,
    Test,
    Properties,
    Io,
    Browser,
    All,
}

#[derive(ClapArgs)]
pub struct Args {
    #[arg(value_enum, default_value = "all")]
    gate: Gate,
    /// Framework checkout to verify
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Print selected members and commands without building
    #[arg(long)]
    list: bool,
    /// Retain the isolated source and target after success
    #[arg(long)]
    keep: bool,
}

struct Check {
    args: Vec<String>,
    tests: bool,
}
fn check(args: &[&str], tests: bool) -> Check {
    Check {
        args: args.iter().map(|s| (*s).into()).collect(),
        tests,
    }
}
fn commands(gate: Gate, root: &Path, target: &Path) -> Vec<Check> {
    let cli = target.join("debug/snap").to_string_lossy().into_owned();
    let root = root.to_string_lossy();
    match gate {
        Gate::Check => vec![
            check(&["cargo", "build", "-p", "snap-cli"], false),
            check(&[&cli, "check", &root, "--workspace"], false),
            check(
                &[
                    "bun",
                    &format!("{root}/node_modules/typescript/bin/tsc"),
                    "--project",
                    &format!("{root}/kits/tsconfig.json"),
                    "--noEmit",
                ],
                false,
            ),
            check(&[&cli, "check-deps", &root], false),
        ],
        Gate::Test => vec![check(
            &[
                "cargo",
                "test",
                "--workspace",
                "--exclude",
                "snap-core-properties",
                "--exclude",
                "snap-browser-tests",
                "--features",
                "snap-identity-native/passkey,snap-transport/native-server",
            ],
            true,
        )],
        Gate::Properties => vec![check(
            &[
                "cargo",
                "test",
                "-p",
                "snap-core-properties",
                "--",
                "--nocapture",
            ],
            true,
        )],
        Gate::Io => vec![
            check(
                &[
                    "cargo",
                    "test",
                    "-p",
                    "snap-cli",
                    "--test",
                    "check",
                    "--test",
                    "check_contract",
                    "--test",
                    "application",
                    "--",
                    "--ignored",
                    "--skip",
                    "fixture_child",
                ],
                true,
            ),
            check(
                &[
                    "cargo",
                    "test",
                    "-p",
                    "snap-platform-tests",
                    "--test",
                    "host_tcp",
                    "--test",
                    "document_sync",
                    "--",
                    "--ignored",
                ],
                true,
            ),
            check(
                &[
                    "cargo",
                    "test",
                    "-p",
                    "snap-transport",
                    "--features",
                    "native-server",
                    "--test",
                    "native_tls",
                    "--",
                    "--ignored",
                ],
                true,
            ),
            check(
                &[
                    "cargo",
                    "test",
                    "-p",
                    "snap-store-sqlite",
                    "--test",
                    "recovery",
                    "--",
                    "--ignored",
                ],
                true,
            ),
            check(
                &[
                    "cargo",
                    "test",
                    "-p",
                    "snap-crypto",
                    "--test",
                    "native",
                    "--test",
                    "token",
                    "--",
                    "--ignored",
                ],
                true,
            ),
        ],
        Gate::Browser => vec![check(
            &[
                "cargo",
                "run",
                "-p",
                "snap-browser-tests",
                "--",
                "--prepare",
                "all",
            ],
            false,
        )],
        Gate::All => unreachable!("expanded before selection"),
    }
}

fn manifest(root: &Path) -> Result<toml::Value> {
    let mut value: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let workspace = value
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
        .context("Missing workspace")?;
    let members = workspace
        .get_mut("members")
        .and_then(toml::Value::as_array_mut)
        .context("Missing workspace members")?;
    members.retain(|member| member.as_str().is_some_and(|p| !p.starts_with("apps/")));
    ensure!(
        !members.is_empty(),
        "No framework workspace members selected"
    );
    let defaults = members
        .iter()
        .filter(|m| !matches!(m.as_str(), Some("tests/properties" | "tests/browser")))
        .cloned()
        .collect();
    workspace.insert("default-members".into(), toml::Value::Array(defaults));
    if let Some(dependencies) = workspace
        .get_mut("dependencies")
        .and_then(toml::Value::as_table_mut)
    {
        dependencies.retain(|_, dep| {
            !dep.get("path")
                .and_then(toml::Value::as_str)
                .is_some_and(|p| p.starts_with("apps/"))
        });
    }
    Ok(value)
}

fn copy(source: &Path, destination: &Path, original: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        // Framework files cannot smuggle app sources in through another path.
        if let Ok(target) = source.canonicalize() {
            ensure!(
                !target.starts_with(original.join("apps")),
                "Framework symlink reaches apps: {}",
                source.display()
            );
        }
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if [
                "target",
                "node_modules",
                ".git",
                ".deployment",
                ".snap",
                "dist",
            ]
            .iter()
            .any(|name| entry.file_name() == *name)
            {
                continue;
            }
            copy(
                &entry.path(),
                &destination.join(entry.file_name()),
                original,
            )?;
        }
    } else {
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn checkout(root: &Path, destination: &Path, manifest: &toml::Value) -> Result<()> {
    fs::create_dir(destination)?;
    for name in ["crates", "kits", "tools", "tests", "bin"] {
        copy(&root.join(name), &destination.join(name), root)?;
    }
    for name in [
        "Cargo.lock",
        "mise.toml",
        "deny.toml",
        "bun.lock",
        "tsconfig.json",
    ] {
        fs::copy(root.join(name), destination.join(name))?;
    }
    fs::write(
        destination.join("Cargo.toml"),
        toml::to_string_pretty(manifest)?,
    )?;
    let mut package: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("package.json"))?)?;
    package
        .as_object_mut()
        .context("Invalid package.json")?
        .remove("workspaces");
    fs::write(
        destination.join("package.json"),
        serde_json::to_vec_pretty(&package)?,
    )?;
    if root.join("node_modules").is_dir() {
        std::os::unix::fs::symlink(root.join("node_modules"), destination.join("node_modules"))?;
    }
    Ok(())
}

fn command(args: &[String], root: &Path, target: &Path, cache: &Path) -> Command {
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", target)
        .env("TMPDIR", cache)
        .env("SNAP_BROWSER_ROOT", root);
    command
}

async fn verify(
    root: &Path,
    target: &Path,
    cache: &Path,
    gates: &[Gate],
    runner: &Runner,
) -> Result<()> {
    let metadata = runner
        .run(
            &mut command(
                &[
                    "cargo".into(),
                    "metadata".into(),
                    "--format-version=1".into(),
                    "--all-features".into(),
                ],
                root,
                target,
                cache,
            ),
            true,
        )
        .await?;
    runner.check()?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata)?;
    for package in metadata["packages"]
        .as_array()
        .context("Missing Cargo packages")?
    {
        if package["source"].is_null() {
            let path = Path::new(
                package["manifest_path"]
                    .as_str()
                    .context("Missing manifest path")?,
            )
            .canonicalize()?;
            ensure!(
                path.starts_with(root),
                "Framework dependency escapes its source copy: {}",
                path.display()
            );
        }
    }
    for gate in gates {
        println!("[{gate:?}]");
        for check in commands(*gate, root, target) {
            println!("+ {}", check.args.join(" "));
            // Capture only test summaries. Compiler and browser output streams normally.
            let mut command = command(&check.args, root, target, cache);
            let output = if check.tests {
                runner.report(&mut command).await
            } else {
                runner.run(&mut command, false).await
            };
            runner.check()?;
            let output = String::from_utf8_lossy(&output?).into_owned();
            if check.tests {
                let count: usize = output
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("test result: ok. ")?
                            .split_whitespace()
                            .next()?
                            .parse::<usize>()
                            .ok()
                    })
                    .sum();
                ensure!(
                    count > 0,
                    "No passing cases selected: {}",
                    check.args.join(" ")
                );
            }
        }
    }
    Ok(())
}

pub async fn run(args: Args, runner: &Runner) -> Result<()> {
    let root = args.root.canonicalize()?;
    let manifest = manifest(&root)?;
    let gates = match args.gate {
        Gate::All => vec![
            Gate::Check,
            Gate::Test,
            Gate::Properties,
            Gate::Io,
            Gate::Browser,
        ],
        gate => vec![gate],
    };
    println!("Framework members: {}", manifest["workspace"]["members"]);
    println!(
        "Shared conformance does not yet cover Wasm execution, browser client carriers, client-side durable recovery or the full Transport/Store matrix."
    );
    if args.list {
        for gate in gates {
            println!("[{gate:?}]");
            for check in commands(gate, Path::new("<framework>"), Path::new("<target>")) {
                println!("{}", check.args.join(" "));
            }
        }
        return Ok(());
    }
    let cache = PathBuf::from(std::env::var_os("HOME").context("HOME is required")?)
        .join(".cache/coding-agents");
    fs::create_dir_all(&cache)?;
    let scratch = tempfile::Builder::new()
        .prefix("snap-framework-")
        .tempdir_in(&cache)?
        .keep();
    println!("Isolated verification: {}", scratch.display());
    let source = scratch.join("source");
    let result = async {
        checkout(&root, &source, &manifest)?;
        verify(&source, &scratch.join("target"), &cache, &gates, runner).await
    }
    .await;
    if let Err(error) = result {
        eprintln!(
            "Verification incomplete; retained source and evidence at {}",
            scratch.display()
        );
        return Err(error);
    }
    runner.check()?;
    println!("Framework verification passed: {gates:?}");
    if !args.keep {
        fs::remove_dir_all(scratch)?;
    }
    Ok(())
}
