//! Cargo owns dependency discovery; notify owns filesystem event delivery.
//! Generated outputs never invalidate source generations. Events during a build
//! accumulate until the coordinator accepts or discards that candidate.
use crate::{config::Project, process::Runner};
use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, sync::mpsc};

#[derive(Clone, Copy, Default, Debug)]
pub struct Changes {
    pub native: bool,
    pub web: bool,
    pub config: bool,
}
impl Changes {
    pub fn merge(&mut self, other: Self) {
        self.native |= other.native;
        self.web |= other.web;
        self.config |= other.config;
    }
}

pub struct Sources {
    _watcher: RecommendedWatcher,
    events: mpsc::UnboundedReceiver<notify::Result<Event>>,
    roots: BTreeMap<PathBuf, Changes>,
    manifests: BTreeSet<PathBuf>,
    ignored: Vec<PathBuf>,
    config: PathBuf,
}

impl Sources {
    pub async fn new(project: &Project, runner: &Runner) -> Result<Self> {
        let (send, events) = mpsc::unbounded_channel();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = send.send(event);
        })?;
        let mut sources = Self {
            _watcher: watcher,
            events,
            roots: BTreeMap::new(),
            manifests: BTreeSet::new(),
            ignored: vec![project.root.join(".snap")],
            config: project.root.join("snap.toml"),
        };
        if let Some(web) = &project.config.web {
            sources.ignored.push(project.path(&web.bindings));
        }
        let mut targets = vec![(&project.config.server.manifest, false)];
        if let Some(web) = &project.config.web {
            targets.push((&web.wasm_manifest, true));
        }
        for (manifest, wasm) in targets {
            let mut command = Command::new("cargo");
            command
                .current_dir(&project.root)
                .args([
                    "metadata",
                    "--format-version=1",
                    "--all-features",
                    "--manifest-path",
                ])
                .arg(project.file(manifest)?);
            if wasm {
                command.args(["--filter-platform", "wasm32-unknown-unknown"]);
            }
            let value: Value = serde_json::from_slice(&runner.run(&mut command, true).await?)?;
            sources.ignored.push(PathBuf::from(
                value["target_directory"]
                    .as_str()
                    .context("Missing target directory")?,
            ));
            let workspace = PathBuf::from(
                value["workspace_root"]
                    .as_str()
                    .context("Missing workspace root")?,
            );
            sources.manifests.insert(workspace.join("Cargo.toml"));
            sources.manifests.insert(workspace.join("Cargo.lock"));
            let selected = crate::cargo::selected_package(&value, &project.file(manifest)?)?;
            let mut pending = vec![selected["id"].as_str().context("Missing package ID")?];
            let mut seen = BTreeSet::new();
            while let Some(id) = pending.pop() {
                if !seen.insert(id) {
                    continue;
                }
                let package = value["packages"]
                    .as_array()
                    .context("Missing packages")?
                    .iter()
                    .find(|p| p["id"] == id)
                    .context("Missing dependency package")?;
                if package["source"].is_null() {
                    let manifest = PathBuf::from(
                        package["manifest_path"]
                            .as_str()
                            .context("Missing manifest")?,
                    );
                    let root = manifest.parent().context("Manifest parent")?.to_owned();
                    sources.manifests.insert(manifest);
                    sources.roots.entry(root).or_default().merge(Changes {
                        native: !wasm,
                        web: wasm,
                        config: false,
                    });
                }
                let node = value["resolve"]["nodes"]
                    .as_array()
                    .context("Missing dependency graph")?
                    .iter()
                    .find(|n| n["id"] == id)
                    .context("Missing graph node")?;
                for dep in node["deps"].as_array().context("Missing dependencies")? {
                    if dep["dep_kinds"]
                        .as_array()
                        .context("Missing kinds")?
                        .iter()
                        .any(|k| k["kind"] != "dev")
                    {
                        pending.push(dep["pkg"].as_str().context("Missing dependency ID")?);
                    }
                }
            }
        }
        let mut directories = BTreeSet::new();
        for root in sources.roots.keys() {
            sources.directories(root, &mut directories)?;
        }
        directories.insert(project.root.clone());
        for manifest in &sources.manifests {
            directories.insert(
                manifest
                    .parent()
                    .context("Missing manifest parent")?
                    .to_owned(),
            );
        }
        // Cargo configuration/toolchain selection may live above the package.
        for ancestor in project.root.ancestors() {
            directories.insert(ancestor.to_owned());
            for name in ["rust-toolchain", "rust-toolchain.toml"] {
                sources.manifests.insert(ancestor.join(name));
            }
            let cargo = ancestor.join(".cargo");
            if cargo.is_dir() {
                directories.insert(cargo.clone());
                sources.manifests.insert(cargo.join("config"));
                sources.manifests.insert(cargo.join("config.toml"));
            }
        }
        for directory in directories {
            sources
                ._watcher
                .watch(&directory, RecursiveMode::NonRecursive)?;
        }
        Ok(sources)
    }

    fn excluded(&self, path: &Path) -> bool {
        self.ignored.iter().any(|p| path.starts_with(p))
            || path.components().any(|p| {
                matches!(
                    p.as_os_str().to_str(),
                    Some(".git" | ".snap" | "target" | "node_modules")
                )
            })
    }

    fn directories(&self, root: &Path, out: &mut BTreeSet<PathBuf>) -> Result<()> {
        if self.excluded(root) || !out.insert(root.to_owned()) {
            return Ok(());
        }
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                self.directories(&entry.path(), out)?;
            }
        }
        Ok(())
    }

    fn classify(&self, event: Event) -> Option<Changes> {
        if matches!(event.kind, EventKind::Access(_)) {
            return None;
        }
        let mut result = Changes::default();
        for path in event.paths {
            if self.excluded(&path) {
                continue;
            }
            if path == self.config
                || self.manifests.contains(&path)
                || path.file_name().is_some_and(|n| n == "Cargo.toml")
            {
                result.merge(Changes {
                    native: true,
                    web: true,
                    config: true,
                });
            } else if !path.extension().is_some_and(|ext| {
                matches!(
                    ext.to_str(),
                    Some("ts" | "tsx" | "js" | "jsx" | "css" | "html" | "md")
                )
            }) {
                // Nested packages override an enclosing application's ownership.
                if let Some((_, changes)) = self
                    .roots
                    .iter()
                    .filter(|(root, _)| path.starts_with(root))
                    .max_by_key(|(root, _)| root.components().count())
                {
                    result.merge(*changes);
                }
            }
        }
        (result.native || result.web || result.config).then_some(result)
    }

    pub async fn next(&mut self) -> Result<Changes> {
        loop {
            let event = self.events.recv().await.context("File watcher stopped")??;
            if let Some(changes) = self.classify(event) {
                return Ok(changes);
            }
        }
    }

    /// Wait for a quiet edit window, including events queued while compiling.
    pub async fn settle(&mut self, mut changes: Changes) -> Result<Changes> {
        while let Ok(next) = tokio::time::timeout(Duration::from_millis(250), self.next()).await {
            changes.merge(next?);
        }
        Ok(changes)
    }
}
