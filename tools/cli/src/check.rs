//! Static checks only. Compilers may analyze test targets, but never run them.
use crate::{
    architecture, cargo,
    config::{CheckVariant, Project},
    process::Runner,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::process::Command;

struct Selection {
    root: PathBuf,
    manifest: PathBuf,
    workspace: bool,
    members: BTreeSet<PathBuf>,
    packages: Vec<String>,
    projects: Vec<Project>,
}

impl Selection {
    async fn discover(
        start: Option<PathBuf>,
        workspace: bool,
        framework: bool,
        runner: &Runner,
    ) -> Result<Self> {
        let start = start.unwrap_or(std::env::current_dir()?);
        let start = start
            .canonicalize()
            .with_context(|| format!("Project directory does not exist: {}", start.display()))?;
        ensure!(
            start.is_dir(),
            "Project path is not a directory: {}",
            start.display()
        );
        // App discovery takes precedence over nested Cargo workspaces. Only use
        // Cargo-only discovery when no enclosing app declaration exists.
        let application = start
            .ancestors()
            .find(|root| root.join("snap.toml").symlink_metadata().is_ok())
            .map(|root| Project::discover(Some(root.to_owned())))
            .transpose()?;
        if !workspace
            && !framework
            && let Some(project) = application
        {
            return Ok(Self {
                manifest: primary_manifest(&project)?,
                root: project.root.clone(),
                workspace: false,
                members: BTreeSet::new(),
                packages: Vec::new(),
                projects: vec![project],
            });
        }
        let manifest = if let Some(project) = &application {
            let (_, metadata) =
                cargo::metadata(project, runner, &primary_manifest(project)?).await?;
            PathBuf::from(
                metadata["workspace_root"]
                    .as_str()
                    .context("Missing workspace root")?,
            )
            .join("Cargo.toml")
        } else {
            let mut workspace_manifest = None;
            for root in start.ancestors() {
                let manifest = root.join("Cargo.toml");
                if manifest.is_file() {
                    let text = std::fs::read_to_string(&manifest)?;
                    let config: toml::Table = toml::from_str(&text)
                        .with_context(|| format!("Invalid {}", manifest.display()))?;
                    if config.contains_key("workspace") {
                        workspace_manifest = Some(manifest);
                        break;
                    }
                }
            }
            workspace_manifest.context(
                "No snap.toml or Cargo workspace found; pass an application or workspace directory",
            )?
        };
        let root = manifest
            .parent()
            .context("Workspace manifest has no parent")?
            .to_owned();
        let metadata = metadata(&root, &manifest, runner).await?;
        let packages = cargo::workspace_packages(&metadata, framework)?;
        if framework {
            ensure!(
                !packages.is_empty(),
                "No framework workspace packages selected"
            );
            return Ok(Self {
                root,
                manifest,
                workspace: true,
                members: BTreeSet::new(),
                packages: packages
                    .iter()
                    .map(|package| {
                        package["name"]
                            .as_str()
                            .context("Missing package name")
                            .map(str::to_owned)
                    })
                    .collect::<Result<_>>()?,
                projects: Vec::new(),
            });
        }
        let mut members = BTreeSet::new();
        let mut roots = BTreeSet::new();
        for package in packages {
            let path = Path::new(
                package["manifest_path"]
                    .as_str()
                    .context("Missing package manifest")?,
            );
            members.insert(path.canonicalize()?);
            // Cargo permits out-of-tree members. Always inspect the member's
            // own directory; only additional ancestors use the workspace bound.
            if let Some(parent) = path.parent()
                && let Some(owner) = std::iter::once(parent)
                    .chain(
                        parent
                            .ancestors()
                            .skip(1)
                            .take_while(|ancestor| ancestor.starts_with(&root)),
                    )
                    .find(|ancestor| ancestor.join("snap.toml").symlink_metadata().is_ok())
            {
                roots.insert(owner.to_owned());
            }
        }
        // A declared standalone child workspace may be below its owning app.
        // Preserve that app even when it lies above the Cargo workspace root.
        if let Some(project) = application {
            roots.insert(project.root);
        }
        // Workspace-level consumer checks are allowed, but never required just
        // to discover a virtual Cargo workspace.
        if root.join("snap.toml").symlink_metadata().is_ok() {
            roots.insert(root.clone());
        }
        let projects = roots
            .into_iter()
            .map(|root| Project::discover(Some(root)))
            .collect::<Result<_>>()?;
        Ok(Self {
            root,
            manifest,
            workspace: true,
            members,
            packages: Vec::new(),
            projects,
        })
    }
}

fn primary_manifest(project: &Project) -> Result<PathBuf> {
    project.file(
        project
            .config
            .check
            .rust
            .first()
            .map_or(Path::new("Cargo.toml"), PathBuf::as_path),
    )
}

async fn metadata(root: &Path, manifest: &Path, runner: &Runner) -> Result<Value> {
    let output = runner
        .run(
            Command::new("cargo")
                .current_dir(root)
                .args([
                    "metadata",
                    "--format-version=1",
                    "--no-deps",
                    "--manifest-path",
                ])
                .arg(manifest),
            true,
        )
        .await?;
    serde_json::from_slice(&output).context("Invalid Cargo metadata")
}
pub async fn run(
    start: Option<PathBuf>,
    runner: &Runner,
    structure_only: bool,
    workspace: bool,
    framework: bool,
) -> Result<()> {
    let selected = Selection::discover(start, workspace, framework, runner).await?;
    let mut manifests = BTreeSet::new();
    if selected.workspace {
        manifests.insert(selected.manifest.clone());
    }
    for project in &selected.projects {
        if project.config.check.rust.is_empty() {
            manifests.insert(project.file(Path::new("Cargo.toml"))?);
        }
        for manifest in &project.config.check.rust {
            manifests.insert(project.file(manifest)?);
        }
        for variant in &project.config.check.variants {
            manifests.insert(project.file(&variant.manifest)?);
        }
    }
    let manifests = manifests.into_iter().collect::<Vec<_>>();
    let extra_manifests = manifests
        .iter()
        .filter(|manifest| **manifest != selected.manifest && !selected.members.contains(*manifest))
        .cloned()
        .collect::<Vec<_>>();
    let structural_manifests = if selected.workspace {
        std::iter::once(selected.manifest.clone())
            .chain(extra_manifests.iter().cloned())
            .collect()
    } else {
        manifests.clone()
    };
    if structure_only {
        architecture::check(
            &selected.root,
            runner,
            &structural_manifests,
            selected.workspace,
            &selected.packages,
        )
        .await?;
        println!("Structural checks passed: {}", selected.root.display());
        return Ok(());
    }
    if selected.workspace {
        let packages: Vec<_> = selected.packages.iter().map(String::as_str).collect();
        rust(&selected.root, &selected.manifest, &packages, None, runner).await?;
    }
    let package_manifests = if selected.workspace {
        &extra_manifests
    } else {
        &manifests
    };
    for manifest in package_manifests {
        let meta = metadata(&selected.root, manifest, runner).await?;
        let name = cargo::selected_package(&meta, manifest)?["name"]
            .as_str()
            .context("Missing package name")?;
        rust(&selected.root, manifest, &[name], None, runner).await?;
    }
    for project in &selected.projects {
        for variant in &project.config.check.variants {
            let manifest = project.file(&variant.manifest)?;
            let meta = metadata(&project.root, &manifest, runner).await?;
            let name = cargo::selected_package(&meta, &manifest)?["name"]
                .as_str()
                .context("Missing package name")?;
            rust(&project.root, &manifest, &[name], Some(variant), runner).await?;
            if let Some(allowed) = &variant.allowed_local_dependencies {
                dependency_variant(project, &manifest, name, variant, allowed, runner).await?;
            }
        }
    }
    architecture::check(
        &selected.root,
        runner,
        &structural_manifests,
        selected.workspace,
        &selected.packages,
    )
    .await?;
    for project in &selected.projects {
        for config in &project.config.check.typescript {
            typescript(project, config, runner).await?;
        }
        for args in &project.config.check.commands {
            eprintln!("Checking {}: {:?}", project.config.application, args);
            let mut command = Command::new(&args[0]);
            command.args(&args[1..]).current_dir(&project.root);
            for key in [
                "SNAP_CHECK_EXECUTABLE",
                "SNAP_CHECK_PACKAGE",
                "SNAP_CHECK_WEB_DIR",
                "SNAP_MASTER_KEY",
            ] {
                command.env_remove(key);
            }
            runner.run(&mut command,false).await.with_context(||format!("Project check failed: {args:?}. Install the command if it is missing; checks are never skipped."))?;
        }
    }
    runner.check()?;
    println!("Static checks passed: {}", selected.root.display());
    Ok(())
}

async fn rust(
    root: &Path,
    manifest: &Path,
    packages: &[&str],
    variant: Option<&CheckVariant>,
    runner: &Runner,
) -> Result<()> {
    for (task, args) in [
        ("fmt", vec!["--", "--check"]),
        ("clippy", vec!["--all-targets", "--", "-D", "warnings"]),
    ] {
        // Formatting is source-based, so checking it once is sufficient.
        if variant.is_some() && task == "fmt" {
            continue;
        }
        eprintln!(
            "Checking {}: cargo {task}{}",
            if packages.is_empty() {
                "workspace".into()
            } else {
                packages.join(", ")
            },
            variant.map_or(String::new(), |v| format!(
                " features={:?}, default_features={}",
                v.features, v.default_features
            ))
        );
        let mut command = Command::new("cargo");
        command
            .current_dir(root)
            .env_remove("SNAP_MASTER_KEY")
            .arg(task)
            .arg("--manifest-path")
            .arg(manifest);
        if packages.is_empty() {
            command.arg(if task == "fmt" {
                "--all"
            } else {
                "--workspace"
            });
        } else {
            for package in packages {
                command.args(["--package", package]);
            }
        }
        if let Some(variant) = variant {
            features(&mut command, variant);
        }
        command.args(args);
        runner
            .run(&mut command, false)
            .await
            .with_context(|| format!("cargo {task} failed for {}", manifest.display()))?;
    }
    Ok(())
}
fn features(command: &mut Command, variant: &CheckVariant) {
    if !variant.default_features {
        command.arg("--no-default-features");
    }
    if !variant.features.is_empty() {
        command.arg("--features").arg(variant.features.join(","));
    }
}

async fn dependency_variant(
    project: &Project,
    manifest: &Path,
    name: &str,
    variant: &CheckVariant,
    allowed: &[String],
    runner: &Runner,
) -> Result<()> {
    let host = runner.run(Command::new("rustc").arg("-vV"), true).await?;
    let host = String::from_utf8(host)?;
    let host = host
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .context("Missing rustc host")?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&project.root)
        .args([
            "metadata",
            "--format-version=1",
            "--filter-platform",
            host,
            "--manifest-path",
        ])
        .arg(manifest);
    features(&mut command, variant);
    let output = runner.run(&mut command, true).await?;
    let metadata: Value = serde_json::from_slice(&output)?;
    let package = cargo::selected_package(&metadata, manifest)?;
    let local = metadata["packages"]
        .as_array()
        .context("Missing packages")?
        .iter()
        .filter(|p| p["source"].is_null())
        .map(|p| p["name"].as_str().context("Missing package name"))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        package["name"] == name,
        "Feature check selected the wrong package"
    );
    // Cargo metadata resolves the workspace as a whole. Cargo tree's selected
    // normal graph preserves the variant's feature isolation instead of treating
    // features enabled by other workspace members as belonging to this package.
    let mut tree = Command::new("cargo");
    tree.current_dir(&project.root)
        .args(["tree", "--manifest-path"])
        .arg(manifest)
        .args([
            "--package",
            name,
            "--target",
            host,
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ]);
    features(&mut tree, variant);
    let output = String::from_utf8(runner.run(&mut tree, true).await?)?;
    for line in output.lines() {
        let Some(dependency) = line.split_whitespace().next() else {
            continue;
        };
        if dependency != name && local.contains(dependency) {
            ensure!(
                allowed.iter().any(|name| name == dependency),
                "{name}: unexpected normal local dependency {dependency} with features {:?}, default_features={}; check the app's check.variants allowlist",
                variant.features,
                variant.default_features
            );
        }
    }
    Ok(())
}

async fn typescript(project: &Project, config: &Path, runner: &Runner) -> Result<()> {
    let config = project.file(config)?;
    let compiler = project
        .root
        .ancestors()
        .map(|root| root.join("node_modules/typescript/bin/tsc"))
        .find(|path| path.is_file());
    let Some(compiler) = compiler else {
        bail!(
            "{}: TypeScript is not installed; install the project's dependencies before snap check",
            project.config.application
        );
    };
    eprintln!(
        "Checking {}: TypeScript {}",
        project.config.application,
        config.display()
    );
    runner.run(Command::new("bun").current_dir(&project.root).env_remove("SNAP_MASTER_KEY").arg(compiler).arg("--noEmit").arg("--project").arg(config),false).await.with_context(||format!("{}: TypeScript check failed. Checks do not prepare generated bindings; if declarations are missing, run snap build --web-only separately.",project.config.application))?;
    Ok(())
}
