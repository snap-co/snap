use crate::{config::Project, process::Runner};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::process::Command;

pub struct Artifact {
    pub path: PathBuf,
    pub bindgen_version: Option<String>,
}

/// Use Cargo's selected package and artifact messages, including custom target dirs.
pub async fn compile(
    project: &Project,
    runner: &Runner,
    manifest: &Path,
    options: &[&str],
    wasm: bool,
) -> Result<Artifact> {
    let (manifest, metadata) = metadata(project, runner, manifest, wasm).await?;
    let package = selected_package(&metadata, &manifest)?;
    let name = package["name"]
        .as_str()
        .context("Missing Cargo package name")?;
    let id = package["id"].as_str().context("Missing Cargo package ID")?;
    let bindgen_version = if wasm {
        Some(bindgen_version(&metadata, id)?)
    } else {
        None
    };
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
        if message["reason"] != "compiler-artifact" || message["package_id"] != id {
            continue;
        }
        let path = if wasm {
            message["filenames"].as_array().and_then(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|file| file.ends_with(".wasm"))
            })
        } else {
            message["executable"].as_str()
        };
        if let Some(path) = path {
            return Ok(Artifact {
                path: path.into(),
                bindgen_version,
            });
        }
    }
    anyhow::bail!(
        "Cargo did not produce the selected {} artifact",
        if wasm { "WASM" } else { "executable" }
    )
}

pub async fn metadata(
    project: &Project,
    runner: &Runner,
    manifest: &Path,
    wasm: bool,
) -> Result<(PathBuf, Value)> {
    let manifest = project.file(manifest)?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&project.root)
        .args(["metadata", "--format-version=1", "--manifest-path"])
        .arg(&manifest);
    if wasm {
        command.args(["--filter-platform", "wasm32-unknown-unknown"]);
    } else {
        command.arg("--no-deps");
    }
    let output = runner.run(&mut command, true).await?;
    let metadata: Value = serde_json::from_slice(&output).context("Invalid Cargo metadata")?;
    Ok((manifest, metadata))
}

pub fn selected_package<'a>(metadata: &'a Value, manifest: &Path) -> Result<&'a Value> {
    metadata["packages"]
        .as_array()
        .context("Missing Cargo packages")?
        .iter()
        .find(|package| {
            package["manifest_path"]
                .as_str()
                .is_some_and(|path| Path::new(path) == manifest)
        })
        .context("Manifest must select a Cargo package, not a virtual workspace")
}

// Other workspace apps may resolve different versions. Only the selected WASM
// package's normal dependency closure determines the binding format we must read.
fn bindgen_version(metadata: &Value, root: &str) -> Result<String> {
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .context("Missing Cargo dependency graph")?;
    let mut pending = vec![root];
    let mut reachable = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let node = nodes
            .iter()
            .find(|node| node["id"] == id)
            .context("Missing Cargo dependency node")?;
        for dep in node["deps"]
            .as_array()
            .context("Missing Cargo dependency edges")?
        {
            if dep["dep_kinds"]
                .as_array()
                .context("Missing Cargo dependency kinds")?
                .iter()
                .any(|kind| kind["kind"].is_null())
            {
                pending.push(
                    dep["pkg"]
                        .as_str()
                        .context("Missing dependency package ID")?,
                );
            }
        }
    }
    let versions: BTreeSet<_> = metadata["packages"]
        .as_array()
        .context("Missing Cargo packages")?
        .iter()
        .filter(|package| {
            package["name"] == "wasm-bindgen"
                && package["id"]
                    .as_str()
                    .is_some_and(|id| reachable.contains(id))
        })
        .filter_map(|package| package["version"].as_str())
        .collect();
    ensure!(
        versions.len() == 1,
        "Selected WASM package must resolve exactly one wasm-bindgen version through normal dependencies; found {versions:?}. Align its wasm-bindgen dependencies."
    );
    Ok(versions.into_iter().next().unwrap().to_owned())
}
