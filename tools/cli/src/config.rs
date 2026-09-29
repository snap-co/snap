use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub application: String,
    pub build: Option<Build>,
    #[serde(default)]
    pub check: Check,
    #[serde(default)]
    pub test: std::collections::BTreeMap<String, TestSuite>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Empty selects the application root Cargo.toml.
    #[serde(default)]
    pub rust: Vec<PathBuf>,
    #[serde(default)]
    pub commands: Vec<Vec<String>>,
}

/// Only target selection varies. Compilation and packaging belong to Snap.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default = "native")]
    pub server: String,
    pub binary: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
}
fn native() -> String {
    "native".into()
}

/// Suites own literal commands, including any host preparation they require.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSuite {
    pub commands: Vec<Vec<String>>,
}

pub struct Project {
    pub root: PathBuf,
    pub config: Config,
}

impl Project {
    pub fn discover(start: Option<PathBuf>) -> Result<Self> {
        let start = start.unwrap_or(std::env::current_dir()?);
        let start = start
            .canonicalize()
            .with_context(|| format!("Project directory does not exist: {}", start.display()))?;
        ensure!(
            start.is_dir(),
            "Project path is not a directory: {}",
            start.display()
        );
        for root in start.ancestors() {
            let path = root.join("snap.toml");
            // A broken symlink or unreadable config takes precedence over ancestors.
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    let text = std::fs::read_to_string(&path)
                        .with_context(|| format!("Cannot read {}", path.display()))?;
                    let config: Config = toml::from_str(&text)
                        .with_context(|| format!("Invalid {}", path.display()))?;
                    ensure!(
                        config.version == 1,
                        "Unsupported snap.toml version {}; expected 1",
                        config.version
                    );
                    ensure!(
                        !config.application.trim().is_empty(),
                        "application must not be empty"
                    );
                    validate_commands("check", &config.check.commands)?;
                    for (name, suite) in &config.test {
                        ensure!(
                            matches!(
                                name.as_str(),
                                "memory" | "native" | "workers" | "browser" | "full"
                            ),
                            "Unknown test platform {name}; use memory, native, workers, browser or full"
                        );
                        ensure!(
                            !suite.commands.is_empty(),
                            "test.{name}.commands must not be empty"
                        );
                        validate_commands(&format!("test.{name}"), &suite.commands)?;
                    }
                    let project = Self {
                        root: root.to_owned(),
                        config,
                    };
                    if project.config.check.rust.is_empty() {
                        project.file(Path::new("Cargo.toml"))?;
                    }
                    for manifest in &project.config.check.rust {
                        project.file(manifest)?;
                    }
                    return Ok(project);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Cannot inspect {}", path.display()));
                }
            }
        }
        bail!(
            "No snap.toml found from {}. Create one at the application root or pass its directory to snap.",
            start.display()
        )
    }

    pub fn file(&self, path: &Path) -> Result<PathBuf> {
        ensure!(
            !path.as_os_str().is_empty(),
            "Configured file path must not be empty"
        );
        let path = self.root.join(path);
        ensure!(
            path.is_file(),
            "Configured file does not exist: {}",
            path.display()
        );
        Ok(path.canonicalize()?)
    }
}

fn validate_commands(name: &str, commands: &[Vec<String>]) -> Result<()> {
    for command in commands {
        ensure!(
            command.first().is_some_and(|s| !s.trim().is_empty()),
            "{name}.commands must contain a nonempty executable"
        );
    }
    Ok(())
}
