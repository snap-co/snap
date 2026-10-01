use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;
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
    /// Additional compiler configurations and normal local-dependency contracts.
    #[serde(default)]
    pub variants: Vec<CheckVariant>,
    /// TypeScript projects, checked without emitting or preparing application assets.
    #[serde(default)]
    pub typescript: Vec<PathBuf>,
    #[serde(default)]
    pub commands: Vec<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckVariant {
    pub manifest: PathBuf,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default = "enabled")]
    pub default_features: bool,
    /// Exact transitive normal path-dependency allowlist, excluding this package.
    /// Registry dependencies and dev/build edges are not part of this contract.
    pub allowed_local_dependencies: Option<Vec<String>>,
}
fn enabled() -> bool {
    true
}

/// App-owned package selection. Cargo's target directory is a compilation cache;
/// deployable artifacts are assembled separately under the application's dist/.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Build {
    pub clients: BTreeMap<String, ClientBuild>,
    pub servers: BTreeMap<String, NativeBuild>,
    pub scripts: BTreeMap<String, ScriptBuild>,
    pub development_client: String,
    pub development_server: String,
}
impl Default for Build {
    fn default() -> Self {
        Self {
            clients: BTreeMap::from([("web".into(), ClientBuild::Web(WebBuild::default()))]),
            servers: BTreeMap::from([("native".into(), NativeBuild::default())]),
            scripts: BTreeMap::new(),
            development_client: "web".into(),
            development_server: "native".into(),
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ClientBuild {
    Web(WebBuild),
    Native(NativeBuild),
}
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WebBuild {
    pub source: PathBuf,
    pub wasm: PathBuf,
    /// Optional browser SDK entry point, relative to the application root.
    pub sdk: Option<PathBuf>,
}
impl Default for WebBuild {
    fn default() -> Self {
        Self {
            source: "web".into(),
            wasm: "web/wasm/Cargo.toml".into(),
            sdk: None,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NativeBuild {
    pub manifest: PathBuf,
    pub binary: Option<String>,
    pub features: Vec<String>,
    pub default_features: bool,
    /// Rust target triples. "host" resolves to rustc's host triple at build time.
    /// Workers need their own runtime adapter; browser Wasm is not a native server.
    pub targets: Vec<String>,
    pub args: Vec<String>,
}
impl Default for NativeBuild {
    fn default() -> Self {
        Self {
            manifest: "server/Cargo.toml".into(),
            binary: None,
            features: Vec::new(),
            default_features: true,
            targets: vec!["host".into()],
            args: Vec::new(),
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptBuild {
    pub source: PathBuf,
    pub target: String,
}
impl Config {
    pub fn build(&self) -> Build {
        self.build.clone().unwrap_or_default()
    }
}
impl Build {
    pub fn web(&self, name: &str) -> Result<&WebBuild> {
        match self.clients.get(name) {
            Some(ClientBuild::Web(web)) => Ok(web),
            _ => bail!("Build client {name} must be a declared web client"),
        }
    }
    pub fn server(&self, name: &str) -> Result<&NativeBuild> {
        self.servers
            .get(name)
            .with_context(|| format!("Unknown build server {name}"))
    }
    pub fn validate(&self) -> Result<()> {
        for name in self.clients.keys().chain(self.servers.keys()) {
            ensure!(artifact_name(name), "Invalid build artifact name {name}");
        }
        for (name, script) in &self.scripts {
            ensure!(
                !name.is_empty()
                    && !matches!(
                        name.as_str(),
                        "." | ".."
                            | "config.toml"
                            | "secrets.enc"
                            | "artifacts.toml"
                            | "clients"
                            | "servers"
                            | "server"
                            | "web"
                    )
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
                "Invalid build script output {name}"
            );
            ensure!(
                matches!(script.target.as_str(), "bun" | "node" | "browser"),
                "Unsupported script target {}",
                script.target
            );
        }
        self.web(&self.development_client)?;
        self.server(&self.development_server)?;
        for native in self
            .servers
            .values()
            .chain(self.clients.values().filter_map(|client| match client {
                ClientBuild::Native(native) => Some(native),
                ClientBuild::Web(_) => None,
            }))
        {
            ensure!(
                !native.targets.is_empty(),
                "Native builds must declare at least one target"
            );
            if let Some(binary) = &native.binary {
                ensure!(artifact_name(binary), "Invalid native binary name {binary}");
            }
        }
        Ok(())
    }
}
pub fn artifact_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
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
                    for variant in &project.config.check.variants {
                        project.file(&variant.manifest)?;
                    }
                    for config in &project.config.check.typescript {
                        project.file(config)?;
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
