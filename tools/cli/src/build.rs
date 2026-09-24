use crate::{
    cargo,
    config::{Project, Web},
    process::Runner,
};
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Clone, Copy)]
pub enum Profile {
    Debug,
    Release,
}

pub enum Mode {
    Build(Profile),
    Dev,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

pub struct Artifacts {
    pub directory: PathBuf,
    pub executable: PathBuf,
    pub web: Option<PathBuf>,
}

/// Both build and dev consume a complete package, never subprocess output text.
pub async fn run(project: &Project, runner: &Runner, mode: Mode) -> Result<Artifacts> {
    let profile = match mode {
        Mode::Build(profile) => profile,
        Mode::Dev => Profile::Debug,
    };
    let work = project.root.join(".snap/build");
    std::fs::create_dir_all(&work)?;
    // Bindings are shared across profiles. Serialize all builds of this project.
    let lock = std::fs::File::create(work.join("lock"))?;
    lock.try_lock()
        .context("Another build is running for this project; retry when it finishes")?;
    let dev_hooks = if matches!(mode, Mode::Dev) {
        project.config.prepare.dev.as_slice()
    } else {
        &[]
    };
    for command in project.config.prepare.build.iter().chain(dev_hooks) {
        runner
            .run(
                Command::new(&command[0])
                    .args(&command[1..])
                    .current_dir(&project.root),
                false,
            )
            .await?;
    }
    let staging = Staging(work.join("staging"));
    if staging.0.exists() {
        std::fs::remove_dir_all(&staging.0)?;
    }
    std::fs::create_dir(&staging.0)?;
    let web = match &project.config.web {
        Some(web) => Some(web_build(project, runner, web, profile, &staging.0).await?),
        None => None,
    };
    let server = &project.config.server;
    let mut options = match (&server.bin, &server.example) {
        (Some(name), _) => vec!["--bin", name.as_str()],
        (_, Some(name)) => vec!["--example", name.as_str()],
        _ => unreachable!("config validates target"),
    };
    if matches!(profile, Profile::Release) {
        options.push("--release");
    }
    let executable = cargo::compile(project, runner, &server.manifest, &options, false)
        .await?
        .path;
    let name = executable
        .file_name()
        .context("Invalid executable artifact name")?;
    std::fs::copy(&executable, staging.0.join(name))?;
    runner.check()?;
    let directory = work.join(profile.name());
    // Publish only after all compilation and packaging succeeds. Generated output only.
    if directory.exists() {
        std::fs::remove_dir_all(&directory)?;
    }
    std::fs::rename(&staging.0, &directory)?;
    Ok(Artifacts {
        executable: directory.join(name),
        web: web.map(|_| directory.join("web")),
        directory,
    })
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn web_build(
    project: &Project,
    runner: &Runner,
    web: &Web,
    profile: Profile,
    package: &Path,
) -> Result<PathBuf> {
    let work = project.root.join(".snap/build");
    std::fs::create_dir_all(&work)?;
    runner
        .run(
            Command::new("bun")
                .current_dir(project.path(&web.package_dir))
                .args(["install", "--frozen-lockfile"]),
            false,
        )
        .await?;
    let mut options = vec!["--lib", "--target", "wasm32-unknown-unknown"];
    if matches!(profile, Profile::Release) {
        options.push("--release");
    }
    let artifact = cargo::compile(project, runner, &web.wasm_manifest, &options, true).await?;
    let version = artifact
        .bindgen_version
        .context("Missing WASM binding version")?;
    let bindgen = bindgen(project, runner, &version).await?;
    let bindings = project.path(&web.bindings);
    let mut command = Command::new(&bindgen);
    command
        .args(["--target", "web", "--out-dir"])
        .arg(&bindings)
        .arg(&artifact.path);
    if matches!(profile, Profile::Debug) {
        command.arg("--debug");
    }
    runner.run(&mut command, false).await?;
    let stem = artifact
        .path
        .file_stem()
        .context("Invalid WASM artifact name")?
        .to_str()
        .context("Non-UTF8 WASM name")?;
    // The browser host explicitly requests this asset name in the current SDK.
    let wasm = bindings.join(format!("{stem}_bg.wasm"));
    ensure!(
        wasm.is_file(),
        "wasm-bindgen did not produce {}",
        wasm.display()
    );
    let out = package.join("web");
    let driver = work.join("build-web.ts");
    std::fs::write(&driver, include_str!("../../../scripts/build-web.ts"))?;
    runner
        .run(
            Command::new("bun")
                .current_dir(&project.root)
                .arg(driver)
                .arg(project.file(&web.application)?)
                .arg(project.file(&web.host)?)
                .arg(project.file(&web.html)?)
                .arg(wasm)
                .arg(&out)
                .arg(profile.name()),
            false,
        )
        .await?;
    Ok(out)
}

async fn bindgen(project: &Project, runner: &Runner, version: &str) -> Result<PathBuf> {
    let tools = project
        .root
        .join(".snap/tools")
        .join(format!("wasm-bindgen-{version}"));
    let bindgen = tools.join("bin/wasm-bindgen");
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for candidate in std::iter::once(bindgen.clone())
        .chain(std::env::split_paths(&paths).map(|dir| dir.join("wasm-bindgen")))
    {
        if candidate.is_file() {
            let output = runner
                .run(Command::new(&candidate).arg("--version"), true)
                .await?;
            if String::from_utf8_lossy(&output).trim() == format!("wasm-bindgen {version}") {
                return Ok(candidate);
            }
        }
    }
    let temp = tools.join("tmp");
    std::fs::create_dir_all(&temp)?;
    runner
        .run(
            Command::new("cargo")
                .current_dir(&project.root)
                .env("TMPDIR", temp)
                .args([
                    "install",
                    "wasm-bindgen-cli",
                    "--version",
                    version,
                    "--locked",
                    "--root",
                ])
                .arg(&tools),
            false,
        )
        .await
        .with_context(|| {
            format!("Install wasm-bindgen-cli {version} or put that version on PATH")
        })?;
    Ok(bindgen)
}
