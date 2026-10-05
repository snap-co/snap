use crate::{
    cargo,
    config::{Build, ClientBuild, NativeBuild, Project, WebBuild},
    process::Runner,
};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Parser)]
pub struct Args {
    #[arg(default_value = "development")]
    pub environment: String,
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Build one browser client without packaging servers or native clients.
    #[arg(long)]
    pub web_only: bool,
    /// Browser client to build with --web-only; defaults to build.development_client.
    #[arg(long, requires = "web_only")]
    pub client: Option<String>,
    /// Build only this declared server (all declared clients are still built).
    #[arg(long)]
    pub server: Option<String>,
    /// Override the configured native target triples. May be repeated.
    #[arg(long)]
    pub target: Vec<String>,
    /// Development-only output. Full generations select the development server/client.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

pub(crate) async fn host_target(runner: &Runner) -> Result<String> {
    let output = runner.run(Command::new("rustc").arg("-vV"), true).await?;
    String::from_utf8(output)?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .context("rustc did not report its host target")
}

fn targets(configured: &[String], host: &str, supported: &BTreeSet<String>) -> Result<Vec<String>> {
    let mut selected = BTreeSet::new();
    for target in configured {
        let target = if target == "host" { host } else { target };
        ensure!(supported.contains(target), "Unknown Rust target {target}");
        ensure!(
            !target.starts_with("wasm"),
            "Native artifacts cannot use {target}; Workers/WASI require an application-owned server runtime adapter"
        );
        ensure!(
            selected.insert(target.to_owned()),
            "Duplicate native target {target}"
        );
    }
    ensure!(
        !selected.is_empty(),
        "Native builds must declare at least one target"
    );
    Ok(selected.into_iter().collect())
}

pub async fn run(args: Args, runner: &Runner) -> Result<()> {
    ensure!(
        crate::config::artifact_name(&args.environment),
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
    ensure!(
        !args.web_only || (args.server.is_none() && args.target.is_empty()),
        "--web-only does not select native servers or targets"
    );
    let internal = args.output.is_some() && !args.web_only;
    ensure!(
        !internal || (args.server.is_none() && args.target.is_empty()),
        "Development generations always use the development server on the host target"
    );
    let deployment = project.root.join(".deployment").join(&args.environment);
    let source = deployment.join("config.toml");
    let dist = project.root.join("dist");
    let output = dist.join(&args.environment);
    // Reject invalid package paths before compiling, and retain the preceding release.
    if !args.web_only && !internal {
        let config = snap_config::Config::<toml::Table>::read(&source)?;
        config.validate_package_data(&output)?;
        ensure!(
            config.host.mode == mode,
            "Environment and host.mode disagree"
        );
    }
    let settings = project.config.build();
    settings.validate()?;
    let selected_web = args
        .client
        .as_deref()
        .unwrap_or(&settings.development_client);
    if args.web_only {
        settings.web(selected_web)?;
    }
    if let Some(server) = &args.server {
        settings.server(server)?;
    }
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
    if args.web_only {
        let web = stage.path().join("web");
        build_web(
            &project,
            settings.web(selected_web)?,
            &web,
            production,
            runner,
        )
        .await?;
        let destination = args
            .output
            .unwrap_or_else(|| output.join("clients").join(selected_web));
        development_output(&project, &web, &destination)?;
        println!("Client assets: {}", destination.display());
        return Ok(());
    }
    let host = host_target(runner).await?;
    let supported = runner
        .run(Command::new("rustc").args(["--print", "target-list"]), true)
        .await?;
    let supported: BTreeSet<String> = String::from_utf8(supported)?
        .lines()
        .map(str::to_owned)
        .collect();
    // Validate every requested triple before building any artifact.
    let configured_targets = |native: &NativeBuild| {
        targets(
            if args.target.is_empty() {
                &native.targets
            } else {
                &args.target
            },
            &host,
            &supported,
        )
    };
    for native in settings
        .servers
        .values()
        .chain(settings.clients.values().filter_map(|client| match client {
            ClientBuild::Native(native) => Some(native),
            ClientBuild::Web(_) => None,
        }))
    {
        configured_targets(native)?;
    }
    let mut artifacts = snap_config::Artifacts {
        version: 1,
        application: project.config.application.clone(),
        clients: Default::default(),
        native_clients: Vec::new(),
        servers: Vec::new(),
    };
    if internal {
        build_web(
            &project,
            settings.web(&settings.development_client)?,
            &stage.path().join("web"),
            production,
            runner,
        )
        .await?;
        let server = settings.server(&settings.development_server)?;
        let executable = build_native(&project, server, &host, production, runner).await?;
        std::fs::copy(executable, stage.path().join("server"))?;
    } else {
        for (name, client) in &settings.clients {
            let directory = PathBuf::from("clients").join(name);
            artifacts.clients.insert(name.clone(), directory.clone());
            match client {
                ClientBuild::Web(web) => {
                    build_web(
                        &project,
                        web,
                        &stage.path().join(&directory),
                        production,
                        runner,
                    )
                    .await?
                }
                ClientBuild::Native(native) => {
                    for target in configured_targets(native)? {
                        let executable =
                            build_native(&project, native, &target, production, runner).await?;
                        let binary = native
                            .binary
                            .as_deref()
                            .unwrap_or(&project.config.application);
                        let destination = directory.join(&target).join(binary);
                        copy_executable(&executable, &stage.path().join(&destination))?;
                        artifacts.native_clients.push(snap_config::NativeArtifact {
                            name: name.clone(),
                            target,
                            executable: destination,
                            args: native.args.clone(),
                        });
                    }
                }
            }
        }
        for (name, native) in &settings.servers {
            if args
                .server
                .as_ref()
                .is_some_and(|selected| selected != name)
            {
                continue;
            }
            for target in configured_targets(native)? {
                let executable =
                    build_native(&project, native, &target, production, runner).await?;
                let destination = PathBuf::from("servers")
                    .join(name)
                    .join(&target)
                    .join("server");
                copy_executable(&executable, &stage.path().join(&destination))?;
                artifacts.servers.push(snap_config::NativeArtifact {
                    name: name.clone(),
                    target,
                    executable: destination,
                    args: native.args.clone(),
                });
            }
        }
    }
    build_scripts(&project, &settings, stage.path(), production, runner).await?;
    if let Some(destination) = args.output {
        development_output(&project, stage.path(), &destination)?;
        println!("Development generation: {}", destination.display());
        return Ok(());
    }
    // Keep configuration at the environment root: moving it beneath a target
    // would change all app-owned config-relative paths (storage, TLS, tools, ...).
    let mut document: toml::Table = toml::from_str(&std::fs::read_to_string(&source)?)
        .map_err(|_| anyhow::anyhow!("Invalid config.toml schema"))?;
    document
        .get_mut("host")
        .and_then(toml::Value::as_table_mut)
        .context("Missing host configuration")?
        .insert(
            "web_dir".into(),
            format!("clients/{}", settings.development_client).into(),
        );
    std::fs::write(
        stage.path().join("config.toml"),
        toml::to_string(&document)?,
    )?;
    let secrets = deployment.join("secrets.enc");
    if secrets.try_exists()? {
        copy_file(&secrets, &stage.path().join("secrets.enc"))?;
    }
    std::fs::write(
        stage.path().join("artifacts.toml"),
        toml::to_string(&artifacts)?,
    )?;
    let config = snap_config::Config::<toml::Table>::read(&stage.path().join("config.toml"))?;
    ensure!(
        config.host.mode == mode,
        "Environment and host.mode disagree"
    );
    config.validate_package_data(&output)?;
    for (name, native) in &settings.servers {
        if args
            .server
            .as_ref()
            .is_some_and(|selected| selected != name)
        {
            continue;
        }
        // Cross-built binaries cannot run here. Validate the exact app schema with
        // a host build of the same package/features; target runtime checks remain
        // deployment-specific. A host target is reused by Cargo's build cache.
        let validator = build_native(&project, native, &host, production, runner).await?;
        runner
            .run(
                Command::new(validator)
                    .args(&native.args)
                    .arg("--check-config")
                    .arg("--config")
                    .arg(stage.path().join("config.toml"))
                    .env_remove("SNAP_MASTER_KEY"),
                false,
            )
            .await?;
    }
    publish(stage.path(), &dist, &args.environment)?;
    println!("Package: {}", output.display());
    Ok(())
}

async fn build_web(
    project: &Project,
    web: &WebBuild,
    output: &Path,
    production: bool,
    runner: &Runner,
) -> Result<()> {
    // The external tool must match the framework's exact Rust binding pin. Check
    // before compiling; native-only builds do not require this executable.
    let framework: toml::Value = toml::from_str(include_str!("../../../Cargo.toml"))?;
    let version = framework["workspace"]["dependencies"]["wasm-bindgen"]
        .as_str()
        .and_then(|pin| pin.strip_prefix('='))
        .context("Framework wasm-bindgen must have an exact version pin")?;
    let tool = runner
        .run(
            Command::new("wasm-bindgen")
                .arg("--version")
                .env_remove("SNAP_MASTER_KEY")
                .current_dir(&project.root),
            true,
        )
        .await
        .with_context(|| format!("Install wasm-bindgen {version} with mise install"))?;
    let actual = String::from_utf8(tool)?;
    ensure!(
        actual.trim() == format!("wasm-bindgen {version}"),
        "Expected wasm-bindgen {version}, got {}; run mise install and use mise exec",
        actual.trim()
    );
    std::fs::create_dir_all(output.join("bindings"))?;
    let (manifest, metadata) = cargo::metadata(project, runner, &web.wasm).await?;
    let package = cargo::selected_package(&metadata, &manifest)?;
    let name = package["name"]
        .as_str()
        .context("Missing Wasm package name")?;
    let library = package["targets"]
        .as_array()
        .context("Missing Wasm targets")?
        .iter()
        .find(|target| {
            target["crate_types"]
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"))
        })
        .context("Browser Wasm package must provide a cdylib")?["name"]
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
        .args(["-p", name, "--lib", "--target", "wasm32-unknown-unknown"]);
    if production {
        command.arg("--release");
    }
    runner.run(&mut command, false).await?;
    runner
        .run(
            Command::new("wasm-bindgen")
                .arg(
                    target
                        .join("wasm32-unknown-unknown")
                        .join(if production { "release" } else { "debug" })
                        .join(format!("{library}.wasm")),
                )
                // TypeScript declarations are enabled by default in the CLI.
                .args(["--target", "web", "--out-dir"])
                .arg(output.join("bindings"))
                .env_remove("SNAP_MASTER_KEY")
                .current_dir(&project.root),
            false,
        )
        .await?;
    let helper = tempfile::Builder::new()
        .prefix(".web-build-")
        .suffix(".mjs")
        .tempfile_in(output)?;
    std::fs::write(helper.path(), include_str!("../web-build.mjs"))?;
    let mode = if production {
        "production"
    } else {
        "development"
    };
    runner
        .run(
            Command::new("bun")
                .arg(helper.path())
                .arg(project.root.join(&web.source))
                .arg(output)
                .arg(mode)
                .arg(library)
                .arg(
                    web.sdk
                        .as_ref()
                        .map(|sdk| project.root.join(sdk))
                        .unwrap_or_default(),
                )
                .env_remove("SNAP_MASTER_KEY")
                .env("NODE_ENV", mode)
                .current_dir(&project.root),
            false,
        )
        .await?;
    Ok(())
}

async fn build_native(
    project: &Project,
    native: &NativeBuild,
    target: &str,
    production: bool,
    runner: &Runner,
) -> Result<PathBuf> {
    let (manifest, metadata) = cargo::metadata(project, runner, &native.manifest).await?;
    let package = cargo::selected_package(&metadata, &manifest)?;
    let name = package["name"]
        .as_str()
        .context("Missing native package name")?;
    let binary = native
        .binary
        .as_deref()
        .unwrap_or(&project.config.application);
    let mut command = Command::new("cargo");
    command
        .current_dir(&project.root)
        .env_remove("SNAP_MASTER_KEY")
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args([
            "-p",
            name,
            "--bin",
            binary,
            "--target",
            target,
            "--message-format=json",
        ]);
    if production {
        command.arg("--release");
    }
    if !native.default_features {
        command.arg("--no-default-features");
    }
    if !native.features.is_empty() {
        command.arg("--features").arg(native.features.join(","));
    }
    let output = runner.run(&mut command, true).await?;
    String::from_utf8(output)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            (value["reason"] == "compiler-artifact" && value["target"]["name"] == binary)
                .then(|| value["executable"].as_str().map(PathBuf::from))
                .flatten()
        })
        .context("Cargo did not produce the native executable")
}

async fn build_scripts(
    project: &Project,
    settings: &Build,
    output: &Path,
    production: bool,
    runner: &Runner,
) -> Result<()> {
    if settings.scripts.is_empty() {
        return Ok(());
    }
    let helper = tempfile::Builder::new()
        .prefix(".script-build-")
        .suffix(".mjs")
        .tempfile_in(output)?;
    std::fs::write(helper.path(), include_str!("../script-build.mjs"))?;
    for (name, script) in &settings.scripts {
        runner
            .run(
                Command::new("bun")
                    .arg(helper.path())
                    .arg(project.file(&script.source)?)
                    .arg(output)
                    .arg(name)
                    .arg(&script.target)
                    .arg(if production {
                        "production"
                    } else {
                        "development"
                    })
                    .env_remove("SNAP_MASTER_KEY")
                    .current_dir(&project.root),
                false,
            )
            .await?;
    }
    Ok(())
}
fn copy_executable(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target.parent().context("Missing artifact directory")?)?;
    copy_file(source, target)
}
fn development_output(project: &Project, source: &Path, target: &Path) -> Result<()> {
    let absolute = std::env::current_dir()?.join(target);
    ensure!(
        absolute != project.root,
        "Output must not be the application root"
    );
    std::fs::create_dir_all(target)?;
    copy_tree(source, target)
}
fn publish(stage: &Path, dist: &Path, environment: &str) -> Result<()> {
    let output = dist.join(environment);
    let previous = dist.join(format!(".previous-{environment}"));
    ensure!(
        !previous.try_exists()?,
        "Previous package recovery directory exists"
    );
    if output.try_exists()? {
        std::fs::rename(&output, &previous)?;
    }
    if let Err(error) = std::fs::rename(stage, &output) {
        if previous.try_exists()? {
            std::fs::rename(&previous, &output)?;
        }
        return Err(error.into());
    }
    if previous.try_exists()? {
        std::fs::remove_dir_all(previous)?;
    }
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
            copy_tree(&entry.path(), &destination)?;
        } else {
            copy_file(&entry.path(), &destination)?;
        }
    }
    Ok(())
}
