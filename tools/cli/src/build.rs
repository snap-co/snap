use crate::{cargo, config::Project, process::Runner};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Parser)]
pub struct Args {
    #[arg(default_value = "development")]
    pub environment: String,
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Build client assets for development tooling, without packaging a server.
    #[arg(long)]
    pub web_only: bool,
    /// Internal development output; without --web-only, builds a complete generation.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

pub async fn run(args: Args, runner: &Runner) -> Result<()> {
    ensure!(
        !args.environment.is_empty()
            && args
                .environment
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "Invalid environment name"
    );
    let project = Project::discover(args.project)?;
    let production = args.environment != "development";
    let mode = if production {
        snap_config::Mode::Production
    } else {
        snap_config::Mode::Development
    };
    ensure!(
        !production || (!args.web_only && args.output.is_none()),
        "Custom outputs are development-only"
    );
    let deployment = project.root.join(".deployment").join(&args.environment);
    let source = deployment.join("config.toml");
    if !args.web_only && args.output.is_none() {
        let config = snap_config::Config::<toml::Table>::read(&source)?;
        config.validate_package_data(&project.root.join("dist").join(&args.environment))?;
        ensure!(
            config.host.mode == mode,
            "Environment and host.mode disagree"
        );
    }
    let dist = project.root.join("dist");
    std::fs::create_dir_all(&dist)?;
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dist.join(format!(".build-{}.lock", args.environment)))?;
    let _lock = nix::fcntl::Flock::lock(lock_file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|_| anyhow::anyhow!("Another build owns this environment"))?;
    let stage = tempfile::Builder::new()
        .prefix(".build-")
        .tempdir_in(&dist)?;
    let web = stage.path().join("web");
    std::fs::create_dir_all(web.join("bindings"))?;
    let wasm_manifest = Path::new("wasm/Cargo.toml");
    let (manifest, metadata) = cargo::metadata(&project, runner, wasm_manifest).await?;
    let package = cargo::selected_package(&metadata, &manifest)?;
    let package_name = package["name"]
        .as_str()
        .context("Missing Wasm package name")?;
    let library = package["targets"]
        .as_array()
        .context("Missing Wasm targets")?
        .iter()
        .find(|t| {
            t["crate_types"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "cdylib"))
        })
        .context("wasm/ must provide a cdylib")?["name"]
        .as_str()
        .context("Missing Wasm library name")?;
    let target = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .context("Missing Cargo target directory")?,
    );
    let mut command = Command::new("cargo");
    command
        .current_dir(&project.root)
        .env_remove("SNAP_MASTER_KEY")
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args([
            "-p",
            package_name,
            "--lib",
            "--target",
            "wasm32-unknown-unknown",
        ]);
    if production {
        command.arg("--release");
    }
    runner.run(&mut command, false).await?;
    let profile = if production { "release" } else { "debug" };
    wasm_bindgen_cli_support::Bindgen::new()
        .typescript(true)
        .input_path(
            target
                .join("wasm32-unknown-unknown")
                .join(profile)
                .join(format!("{library}.wasm")),
        )
        .web(true)?
        .generate(web.join("bindings"))?;
    let helper = stage.path().join("web-build.mjs");
    std::fs::write(&helper, include_str!("../web-build.mjs"))?;
    runner
        .run(
            Command::new("bun")
                .arg(&helper)
                .arg(&project.root)
                .arg(&web)
                .arg(if production {
                    "production"
                } else {
                    "development"
                })
                .arg(if args.web_only { "web" } else { "package" })
                .arg(library)
                .env_remove("SNAP_MASTER_KEY")
                .env(
                    "NODE_ENV",
                    if production {
                        "production"
                    } else {
                        "development"
                    },
                )
                .current_dir(&project.root),
            false,
        )
        .await?;
    std::fs::remove_file(helper)?;
    if args.web_only {
        let output = args
            .output
            .unwrap_or_else(|| project.root.join(".snap/web"));
        ensure!(
            output != project.root,
            "Output must not be the application root"
        );
        std::fs::create_dir_all(&output)?;
        copy_tree(&web, &output)?;
        println!("Client assets: {}", output.display());
        return Ok(());
    }
    let settings = project.config.build.as_ref();
    let server = settings.map_or("native", |s| s.server.as_str());
    ensure!(
        matches!(server, "native" | "local"),
        "build.server must be native or local"
    );
    let server_manifest = PathBuf::from(server).join("Cargo.toml");
    let (manifest, metadata) = cargo::metadata(&project, runner, &server_manifest).await?;
    let package = cargo::selected_package(&metadata, &manifest)?;
    let name = package["name"]
        .as_str()
        .context("Missing server package name")?;
    let binary = settings
        .and_then(|s| s.binary.as_deref())
        .unwrap_or(&project.config.application);
    let mut command = Command::new("cargo");
    command
        .current_dir(&project.root)
        .env_remove("SNAP_MASTER_KEY")
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args(["-p", name, "--bin", binary, "--message-format=json"]);
    if production {
        command.arg("--release");
    }
    if let Some(settings) = settings.filter(|s| !s.features.is_empty()) {
        command
            .args(["--no-default-features", "--features"])
            .arg(settings.features.join(","));
    }
    let output = runner.run(&mut command, true).await?;
    let executable = String::from_utf8(output)?
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            (v["target"]["name"] == binary)
                .then(|| v["executable"].as_str().map(PathBuf::from))
                .flatten()
        })
        .context("Cargo did not produce the server executable")?;
    let packaged_server = stage.path().join("server");
    std::fs::copy(&executable, &packaged_server)?;
    if let Some(binary) = settings.and_then(|s| s.cli.as_deref()) {
        ensure!(
            !binary.is_empty()
                && !matches!(binary, "server" | "web")
                && binary
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "build.cli must be a binary name"
        );
        if binary
            == settings
                .and_then(|s| s.binary.as_deref())
                .unwrap_or(&project.config.application)
        {
            // A combined client/server command needs no second build or CLI target.
            std::fs::copy(&executable, stage.path().join(binary))?;
        } else {
            let (manifest, metadata) =
                cargo::metadata(&project, runner, Path::new("cli/Cargo.toml")).await?;
            let package = cargo::selected_package(&metadata, &manifest)?;
            let name = package["name"]
                .as_str()
                .context("Missing CLI package name")?;
            let mut command = Command::new("cargo");
            command
                .current_dir(&project.root)
                .env_remove("SNAP_MASTER_KEY")
                .args(["build", "--locked", "--manifest-path"])
                .arg(manifest)
                .args(["-p", name, "--bin", binary, "--message-format=json"]);
            if production {
                command.arg("--release");
            }
            let output = runner.run(&mut command, true).await?;
            let executable = String::from_utf8(output)?
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find_map(|v| {
                    (v["target"]["name"] == binary)
                        .then(|| v["executable"].as_str().map(PathBuf::from))
                        .flatten()
                })
                .context("Cargo did not produce the CLI executable")?;
            std::fs::copy(executable, stage.path().join(binary))?;
        }
    }
    if let Some(output) = args.output {
        ensure!(
            std::env::current_dir()?.join(&output) != project.root,
            "Output must not be the application root"
        );
        std::fs::create_dir_all(&output)?;
        copy_tree(stage.path(), &output)?;
        println!("Development generation: {}", output.display());
        return Ok(());
    }
    copy_file(&source, &stage.path().join("config.toml"))?;
    let secrets = deployment.join("secrets.enc");
    if secrets.try_exists()? {
        copy_file(&secrets, &stage.path().join("secrets.enc"))?;
    }
    let output = dist.join(&args.environment);
    let packaged_config =
        snap_config::Config::<toml::Table>::read(&stage.path().join("config.toml"))?;
    ensure!(
        packaged_config.host.mode == mode,
        "Environment and host.mode disagree"
    );
    packaged_config.validate_package_data(&output)?;
    // Check the actual staged schema and bag, not possibly changing source inputs.
    // This never opens a database, listener, or decrypts secrets.
    runner
        .run(
            Command::new(&packaged_server)
                .args(settings.map_or(&[][..], |s| s.server_args.as_slice()))
                .arg("--check-config")
                .arg("--config")
                .arg(stage.path().join("config.toml"))
                .env_remove("SNAP_MASTER_KEY"),
            false,
        )
        .await?;
    // Publish only a completed package. Keep the preceding one on build failure.
    let previous = dist.join(format!(".previous-{}", args.environment));
    ensure!(
        !previous.try_exists()?,
        "Previous package recovery directory exists"
    );
    if output.try_exists()? {
        std::fs::rename(&output, &previous)?;
    }
    if let Err(error) = std::fs::rename(stage.path(), &output) {
        if previous.try_exists()? {
            std::fs::rename(&previous, &output)?;
        }
        return Err(error.into());
    }
    if previous.try_exists()? {
        std::fs::remove_dir_all(previous)?;
    }
    println!("Package: {}", output.display());
    Ok(())
}
fn copy_file(source: &Path, target: &Path) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(source)?.file_type().is_file(),
        "Package input must be a regular file"
    );
    std::fs::copy(source, target)?;
    Ok(())
}
pub(crate) fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&destination)?;
            copy_tree(&entry.path(), &destination)?;
        } else {
            copy_file(&entry.path(), &destination)?;
        }
    }
    Ok(())
}
