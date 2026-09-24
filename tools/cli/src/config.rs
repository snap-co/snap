use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub application: String,
    pub server: Server,
    pub web: Option<Web>,
    #[serde(default)]
    pub prepare: Prepare,
    #[serde(default)]
    pub dev: Dev,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub manifest: PathBuf,
    pub bin: Option<String>,
    pub example: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Web {
    /// Directory containing the JS package manifest and lockfile.
    pub package_dir: PathBuf,
    pub application: PathBuf,
    pub host: PathBuf,
    pub html: PathBuf,
    /// Cargo manifest for the application's WASM binding crate.
    pub wasm_manifest: PathBuf,
    /// Output location imported by the project's TypeScript facade.
    pub bindings: PathBuf,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    #[serde(default)]
    pub build: Vec<Vec<String>>,
    #[serde(default)]
    pub dev: Vec<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dev {
    pub address: SocketAddr,
}
impl Default for Dev {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:3846".parse().expect("default address"),
        }
    }
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
            // Even a broken symlink or unreadable config takes precedence over ancestors.
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
                    ensure!(
                        matches!(
                            (&config.server.bin, &config.server.example),
                            (Some(_), None) | (None, Some(_))
                        ),
                        "server must select exactly one bin or example"
                    );
                    let name = config
                        .server
                        .bin
                        .as_ref()
                        .or(config.server.example.as_ref())
                        .unwrap();
                    ensure!(!name.trim().is_empty(), "server target must not be empty");
                    for (name, commands) in [
                        ("build", &config.prepare.build),
                        ("dev", &config.prepare.dev),
                    ] {
                        for command in commands {
                            ensure!(
                                command.first().is_some_and(|s| !s.trim().is_empty()),
                                "prepare.{name} commands must contain a nonempty executable"
                            );
                        }
                    }
                    let project = Self {
                        root: root.to_owned(),
                        config,
                    };
                    project.file(&project.config.server.manifest)?;
                    if let Some(web) = &project.config.web {
                        for path in [&web.application, &web.host, &web.html, &web.wasm_manifest] {
                            project.file(path)?;
                        }
                        project.file(&web.package_dir.join("package.json"))?;
                        ensure!(
                            !web.bindings.as_os_str().is_empty(),
                            "web.bindings must not be empty"
                        );
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
            "No snap.toml found from {}. Create one at the application root or pass its directory to snap build or snap dev.",
            start.display()
        )
    }

    pub fn path(&self, path: &Path) -> PathBuf {
        self.root.join(path)
    }
    pub fn address(&self) -> Result<SocketAddr> {
        match std::env::var("SNAP_ADDR") {
            Ok(address) => address
                .parse()
                .context("SNAP_ADDR must be an IP address and port"),
            Err(std::env::VarError::NotPresent) => Ok(self.config.dev.address),
            Err(error) => Err(error.into()),
        }
    }
    pub fn file(&self, path: &Path) -> Result<PathBuf> {
        ensure!(
            !path.as_os_str().is_empty(),
            "Configured file path must not be empty"
        );
        let path = self.path(path);
        ensure!(
            path.is_file(),
            "Configured file does not exist: {}",
            path.display()
        );
        Ok(path.canonicalize()?)
    }
}
