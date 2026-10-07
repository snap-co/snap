use crate::{config::Project, process::Runner};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub async fn metadata(
    project: &Project,
    runner: &Runner,
    manifest: &Path,
) -> Result<(PathBuf, Value)> {
    let manifest = project.file(manifest)?;
    let output = runner
        .run(
            Command::new("cargo")
                .current_dir(&project.root)
                .env_remove("SNAP_MASTER_KEY")
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

pub fn workspace_packages(metadata: &Value, framework: bool) -> Result<Vec<&Value>> {
    let members = metadata["workspace_members"]
        .as_array()
        .context("Missing workspace members")?;
    let apps = Path::new(
        metadata["workspace_root"]
            .as_str()
            .context("Missing workspace root")?,
    )
    .join("apps");
    metadata["packages"]
        .as_array()
        .context("Missing Cargo packages")?
        .iter()
        .filter(|package| members.contains(&package["id"]))
        .filter_map(|package| {
            let path = package["manifest_path"].as_str();
            if framework && path.is_some_and(|path| Path::new(path).starts_with(&apps)) {
                None
            } else {
                Some(path.context("Missing package manifest").map(|_| package))
            }
        })
        .collect()
}
