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
    /// Dev has already run its hooks before discovering watch inputs.
    PreparedDev(std::fs::File),
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
    let (profile, _lock) = match mode {
        Mode::Build(profile) => {
            let lock = lock(project)?;
            hooks(project, runner, &project.config.prepare.build).await?;
            (profile, lock)
        }
        Mode::PreparedDev(lock) => (Profile::Debug, lock),
    };
    let work = project.root.join(".snap/build");
    let staging = Staging(work.join("staging"));
    if staging.0.exists() {
        std::fs::remove_dir_all(&staging.0)?;
    }
    std::fs::create_dir(&staging.0)?;
    let web = match &project.config.web {
        Some(web) => Some(web_build(project, runner, web, profile, &staging.0, None).await?),
        None => None,
    };
    let executable = native_build(project, runner, profile).await?;
    let name = executable
        .file_name()
        .context("Invalid executable artifact name")?;
    std::fs::copy(&executable, staging.0.join(name))?;
    runner.check()?;
    let directory = work.join(profile.name());
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

pub async fn prepare_dev(project: &Project, runner: &Runner) -> Result<std::fs::File> {
    let lock = lock(project)?;
    hooks(project, runner, &project.config.prepare.build).await?;
    hooks(project, runner, &project.config.prepare.dev).await?;
    Ok(lock)
}

fn lock(project: &Project) -> Result<std::fs::File> {
    let work = project.root.join(".snap/build");
    std::fs::create_dir_all(&work)?;
    // The lock spans preparation, graph discovery, compilation and packaging.
    let lock = std::fs::File::create(work.join("lock"))?;
    lock.try_lock()
        .context("Another build is running for this project; retry when it finishes")?;
    Ok(lock)
}

async fn hooks(project: &Project, runner: &Runner, commands: &[Vec<String>]) -> Result<()> {
    for command in commands {
        runner
            .run(
                Command::new(&command[0])
                    .args(&command[1..])
                    .current_dir(&project.root),
                false,
            )
            .await?;
    }
    Ok(())
}

async fn native_build(project: &Project, runner: &Runner, profile: Profile) -> Result<PathBuf> {
    let server = &project.config.server;
    let mut options = match (&server.bin, &server.example) {
        (Some(name), _) => vec!["--bin", name.as_str()],
        (_, Some(name)) => vec!["--example", name.as_str()],
        _ => unreachable!("config validates target"),
    };
    if matches!(profile, Profile::Release) {
        options.push("--release");
    }
    Ok(
        cargo::compile(project, runner, &server.manifest, &options, false)
            .await?
            .path,
    )
}

/// A reload candidate has private bindings/assets until the coordinator has
/// checked for newer edits and started its native host. Dropping it removes only
/// that generation; the previous service and browser keep their working files.
pub struct Generation {
    pub artifacts: Artifacts,
    bindings: Option<PathBuf>,
    _cleanup: Staging,
}

impl Generation {
    pub fn publish_bindings(&self, project: &Project) -> Result<()> {
        if let (Some(from), Some(web)) = (&self.bindings, &project.config.web) {
            let to = project.path(&web.bindings);
            if to.exists() {
                std::fs::remove_dir_all(&to)?;
            }
            copy_tree(from, &to)?;
        }
        Ok(())
    }
}

pub async fn reload(
    project: &Project,
    runner: &Runner,
    changes: crate::watch::Changes,
    previous: &Artifacts,
) -> Result<Generation> {
    let _lock = lock(project)?;
    for command in &project.config.prepare.build {
        runner
            .run(
                Command::new(&command[0])
                    .args(&command[1..])
                    .current_dir(&project.root),
                false,
            )
            .await?;
    }
    let directory = project.root.join(".snap/dev").join(format!(
        "generation-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory)?;
    let cleanup = Staging(directory.clone());
    let bindings = if changes.web && project.config.web.is_some() {
        Some(directory.join(".bindings"))
    } else {
        None
    };
    let web = if let Some(web) = &project.config.web {
        if changes.web {
            Some(
                web_build(
                    project,
                    runner,
                    web,
                    Profile::Debug,
                    &directory,
                    bindings.as_deref(),
                )
                .await?,
            )
        } else {
            let target = directory.join("web");
            copy_tree(
                previous
                    .web
                    .as_ref()
                    .context("Missing previous browser assets")?,
                &target,
            )?;
            Some(target)
        }
    } else {
        None
    };
    let executable = if changes.native {
        native_build(project, runner, Profile::Debug).await?
    } else {
        previous.executable.clone()
    };
    let executable_copy =
        directory.join(executable.file_name().context("Missing executable name")?);
    std::fs::copy(executable, &executable_copy)?;
    runner.check()?;
    Ok(Generation {
        artifacts: Artifacts {
            directory,
            executable: executable_copy,
            web,
        },
        bindings,
        _cleanup: cleanup,
    })
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
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
    staged_bindings: Option<&Path>,
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
    let bindings = staged_bindings
        .map(Path::to_owned)
        .unwrap_or_else(|| project.path(&web.bindings));
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
                .arg(profile.name())
                .arg(project.path(&web.bindings))
                .arg(&bindings),
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
