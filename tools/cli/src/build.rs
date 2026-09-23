use crate::{
    config::{Project, Web},
    process::Runner,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::process::Command;

const BINDGEN_VERSION: &str = "0.2.128";

/// Use Cargo's artifact messages, not assumptions about target directories or names.
pub async fn cargo(
    project: &Project,
    runner: &Runner,
    manifest: &Path,
    options: &[&str],
    wasm: bool,
) -> Result<PathBuf> {
    let manifest = project.file(manifest)?;
    let output = runner
        .run(
            Command::new("cargo")
                .current_dir(&project.root)
                .args([
                    "metadata",
                    "--format-version=1",
                    "--no-deps",
                    "--manifest-path",
                ])
                .arg(&manifest),
            true,
        )
        .await?;
    let metadata: Value = serde_json::from_slice(&output).context("Invalid Cargo metadata")?;
    let package = metadata["packages"]
        .as_array()
        .context("Missing Cargo packages")?
        .iter()
        .find(|package| {
            package["manifest_path"]
                .as_str()
                .is_some_and(|path| Path::new(path) == manifest)
        })
        .context("Manifest must select a Cargo package, not a virtual workspace")?;
    let name = package["name"]
        .as_str()
        .context("Missing Cargo package name")?;
    let id = &package["id"];
    let output = runner
        .run(
            Command::new("cargo")
                .current_dir(&project.root)
                .args(["build", "--manifest-path"])
                .arg(&manifest)
                .args([
                    "--package",
                    name,
                    "--message-format=json-render-diagnostics",
                ])
                .args(options),
            true,
        )
        .await?;
    for line in output.split(|byte| *byte == b'\n') {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" || &message["package_id"] != id {
            continue;
        }
        if wasm {
            if let Some(file) = message["filenames"].as_array().and_then(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|file| file.ends_with(".wasm"))
            }) {
                return Ok(file.into());
            }
        } else if let Some(executable) = message["executable"].as_str() {
            return Ok(executable.into());
        }
    }
    anyhow::bail!(
        "Cargo did not produce the selected {} artifact",
        if wasm { "WASM" } else { "executable" }
    )
}

pub async fn web(project: &Project, runner: &Runner, web: &Web) -> Result<PathBuf> {
    let work = project.root.join(".snap/dev");
    std::fs::create_dir_all(&work)?;
    runner
        .run(
            Command::new("bun")
                .current_dir(project.path(&web.package_dir))
                .args(["install", "--frozen-lockfile"]),
            false,
        )
        .await?;
    let artifact = cargo(
        project,
        runner,
        &web.wasm_manifest,
        &["--lib", "--target", "wasm32-unknown-unknown", "--release"],
        true,
    )
    .await?;
    let tools = work.join("tools");
    let bindgen = tools.join("bin/wasm-bindgen");
    let installed = if bindgen.is_file() {
        let output = runner
            .run(Command::new(&bindgen).arg("--version"), true)
            .await?;
        String::from_utf8_lossy(&output).trim() == format!("wasm-bindgen {BINDGEN_VERSION}")
    } else {
        false
    };
    if !installed {
        let temp = work.join("tmp");
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
                        BINDGEN_VERSION,
                        "--locked",
                        "--root",
                    ])
                    .arg(&tools),
                false,
            )
            .await?;
    }
    let bindings = project.path(&web.bindings);
    runner
        .run(
            Command::new(&bindgen)
                .args(["--target", "web", "--out-dir"])
                .arg(&bindings)
                .arg(&artifact),
            false,
        )
        .await?;
    let stem = artifact
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
    let out = work.join("web");
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
                .arg(&out),
            false,
        )
        .await?;
    Ok(out)
}
