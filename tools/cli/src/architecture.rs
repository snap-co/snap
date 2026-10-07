//! Structural policy for explicitly declared project packages, checked through Cargo.
use crate::process::Runner;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Role {
    /// Store and Transport: the barrier application and platform code cross.
    /// Nothing above them may construct a driver or name a table directly.
    Interface,
    /// Snap modules built on the barrier, exposing server SDKs and, where a
    /// Transport operation exists, a client SDK alongside it.
    Core,
    Application,
    /// Drivers and platform capabilities implementing the barrier.
    Platform,
    /// The application's entrypoint: selects platform drivers and connects them
    /// to the definitions the application plugs into core modules with. Owned by
    /// the application, never by snap.
    Host,
    Tool,
}

impl Role {
    fn portable(self) -> bool {
        matches!(self, Self::Interface | Self::Core | Self::Application)
    }
    fn allows(self, target: Self, kind: &str) -> bool {
        use Role::*;
        // Reusable code never selects an app, including through tests/build scripts.
        if matches!(self, Interface | Core | Platform | Tool)
            && matches!(target, Application | Host)
        {
            return false;
        }
        // Host-side build dependencies and test fixtures need not be portable.
        if kind != "normal" {
            return true;
        }
        match self {
            Interface => target == Interface,
            Core => matches!(target, Interface | Core),
            Application => matches!(target, Interface | Core | Application),
            Platform => matches!(target, Interface | Core | Platform),
            Tool => matches!(target, Interface | Core | Platform | Tool),
            Host => true,
        }
    }
}

fn role(package: &Value) -> Result<Option<Role>> {
    if !package["source"].is_null() {
        return Ok(None);
    }
    let name = package["name"].as_str().unwrap_or("unknown package");
    serde_json::from_value(package["metadata"]["snap"]["role"].clone())
        .map(Some)
        .with_context(|| format!("{name}: declare package.metadata.snap.role as interface, core, application, platform, host, or tool in {}", package["manifest_path"]))
}

pub async fn check(
    root: &Path,
    runner: &Runner,
    manifests: &[PathBuf],
    workspace: bool,
    selected_packages: &[String],
) -> Result<()> {
    let version = runner.run(Command::new("rustc").arg("-vV"), true).await?;
    let version = String::from_utf8(version)?;
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .context("rustc did not report its host target")?;
    let mut portable = BTreeMap::new();
    let mut violations = BTreeSet::new();
    let roots: BTreeSet<_> = manifests
        .iter()
        .map(|path| {
            root.join(path)
                .canonicalize()
                .with_context(|| format!("Cannot read manifest {}", path.display()))
        })
        .collect::<Result<_>>()?;
    let mut pending_manifests: Vec<_> = roots.iter().cloned().collect();
    let mut checked_manifests = BTreeSet::new();
    while let Some(manifest) = pending_manifests.pop() {
        if !checked_manifests.insert(manifest.clone()) {
            continue;
        }
        for target in [host, "wasm32-unknown-unknown", "wasm32v1-none"] {
            let output = runner
                .run(
                    Command::new("cargo")
                        .current_dir(root)
                        .args([
                            "metadata",
                            "--format-version=1",
                            "--all-features",
                            "--filter-platform",
                            target,
                            "--manifest-path",
                        ])
                        .arg(&manifest),
                    true,
                )
                .await?;
            let metadata: Value = serde_json::from_slice(&output)?;
            let packages: BTreeMap<_, _> = metadata["packages"]
                .as_array()
                .context("Missing Cargo packages")?
                .iter()
                .map(|p| Ok((p["id"].as_str().context("Missing package id")?, p)))
                .collect::<Result<_>>()?;
            let nodes: BTreeMap<_, _> = metadata["resolve"]["nodes"]
                .as_array()
                .context("Missing dependency graph")?
                .iter()
                .map(|p| Ok((p["id"].as_str().context("Missing node id")?, p)))
                .collect::<Result<_>>()?;
            let mut pending: Vec<&str> = if workspace && roots.contains(&manifest) {
                metadata["workspace_members"]
                    .as_array()
                    .context("Missing workspace members")?
                    .iter()
                    .filter(|id| {
                        selected_packages.is_empty()
                            || packages.get(id.as_str().unwrap_or_default()).is_some_and(
                                |package| {
                                    package["name"].as_str().is_some_and(|name| {
                                        selected_packages.iter().any(|selected| selected == name)
                                    })
                                },
                            )
                    })
                    .filter_map(Value::as_str)
                    .collect()
            } else {
                vec![
                    crate::cargo::selected_package(&metadata, &manifest)?["id"]
                        .as_str()
                        .context("Missing selected package id")?,
                ]
            };
            let mut seen = BTreeSet::new();
            while let Some(id) = pending.pop() {
                if !seen.insert(id) {
                    continue;
                }
                let package = packages.get(id).context("Missing dependency package")?;
                let source_role = role(package)?;
                if source_role.is_some_and(Role::portable) {
                    ensure!(
                        package["targets"]
                            .as_array()
                            .context("Missing Cargo targets")?
                            .iter()
                            .any(|t| t["kind"].as_array().is_some_and(|k| k.iter().any(
                                |v| matches!(
                                    v.as_str(),
                                    Some("lib" | "rlib" | "dylib" | "staticlib" | "cdylib")
                                )
                            ))),
                        "{}: portable packages need a library target; move host binaries into host packages",
                        package["name"]
                    );
                    let portable_manifest = PathBuf::from(
                        package["manifest_path"]
                            .as_str()
                            .context("Missing manifest")?,
                    );
                    // --all-features on an app does not enable every feature of a
                    // path dependency in another workspace. Inspect the same package
                    // selection that its later portable compilation will use.
                    pending_manifests.push(portable_manifest.clone());
                    portable.insert(
                        portable_manifest,
                        package["name"].as_str().context("Missing name")?.to_owned(),
                    );
                }
                let node = nodes.get(id).context("Missing dependency node")?;
                for dep in node["deps"].as_array().context("Missing dependencies")? {
                    let dep_id = dep["pkg"].as_str().context("Missing dependency id")?;
                    pending.push(dep_id);
                    let dependency = packages.get(dep_id).context("Missing dependency")?;
                    if let (Some(source_role), Some(target_role)) = (source_role, role(dependency)?)
                    {
                        for kind in dep["dep_kinds"]
                            .as_array()
                            .context("Missing dependency kinds")?
                        {
                            let kind = kind["kind"].as_str().unwrap_or("normal");
                            if !source_role.allows(target_role, kind) {
                                violations.insert(format!("{} ({source_role:?}) -> {} ({target_role:?}), {kind} dependency on {target}: modules and platforms reach the platform only through the Store/Transport barrier and module SDKs; move driver selection and host assembly to an app-owned host package, or move the behavior into core. Declared in {}",
                                    package["name"], dependency["name"], package["manifest_path"]));
                            }
                        }
                    }
                }
            }
        }
    }
    ensure!(
        violations.is_empty(),
        "Dependency direction violations:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
    for (manifest, name) in portable {
        eprintln!("Checking portable {name}: wasm32v1-none, all features");
        runner.run(Command::new("cargo").current_dir(root)
            .args(["check", "--lib", "--all-features", "--target", "wasm32v1-none", "--package", &name, "--manifest-path"])
            .arg(&manifest), false).await
            .with_context(|| format!("{name}: portable wasm32v1-none check failed. Keep core/application code no_std + alloc; move concrete IO to a platform package."))?;
    }
    runner.check()
}
