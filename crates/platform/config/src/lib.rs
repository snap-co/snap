//! Host-owned startup configuration. Portable modules receive resolved inputs,
//! never this loader, filesystem paths to secrets, or ambient configuration.
mod secrets;
use anyhow::{Context, Result, bail, ensure};
pub use secrets::{MasterKey, Secret, SecretRef, Secrets};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Development,
    Production,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub mode: Mode,
    pub listen: SocketAddr,
    pub origin: Option<String>,
    pub data_dir: PathBuf,
    #[serde(default = "database_name")]
    pub database: PathBuf,
    #[serde(default = "web_name")]
    pub web_dir: PathBuf,
    #[serde(default)]
    pub dev_origins: Vec<String>,
    #[serde(default)]
    pub dev_client_origins: BTreeMap<String, Vec<String>>,
}
/// Optional native binary listener. Credentials always travel over verified TLS.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tcp {
    pub listen: SocketAddr,
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}
fn database_name() -> PathBuf {
    "store.sqlite".into()
}
fn web_name() -> PathBuf {
    "web".into()
}

/// A private, ignored development profile overrides the tracked template.
/// An unreadable or malformed selected profile must never fall back to another one.
pub fn development_config(project: &Path) -> Result<PathBuf> {
    let private = project.join(".snap/development/config.toml");
    Ok(if private.try_exists()? {
        private
    } else {
        project.join(".deployment/development/config.toml")
    })
}

/// Find this application's checkout from the working directory or executable.
/// This supports nested source directories and ordinary target/debug builds without
/// embedding the build machine's source path into a relocatable executable.
pub fn application_development_config(application: &str) -> Result<Option<PathBuf>> {
    ensure!(
        !application.is_empty()
            && application
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "Invalid application name"
    );
    let current = std::env::current_dir()?;
    let executable = std::env::current_exe()?;
    for start in [
        &current,
        executable
            .parent()
            .context("Executable directory missing")?,
    ] {
        for ancestor in start.ancestors() {
            for project in [ancestor.to_owned(), ancestor.join("apps").join(application)] {
                let marker = project.join("snap.toml");
                let text = match std::fs::read_to_string(&marker) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error).context("Cannot read snap.toml"),
                };
                let document: toml::Table = toml::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("Invalid snap.toml schema"))?;
                if document.get("application").and_then(toml::Value::as_str) == Some(application) {
                    return development_config(&project).map(Some);
                }
            }
        }
    }
    Ok(None)
}

/// Paths in a deployment inventory are relative to its environment root. All
/// target binaries share that root's configuration and client assets, so moving
/// the complete package preserves app-owned config-relative resource paths.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifacts {
    pub version: u32,
    pub application: String,
    pub clients: BTreeMap<String, PathBuf>,
    pub native_clients: Vec<NativeArtifact>,
    pub servers: Vec<NativeArtifact>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeArtifact {
    pub name: String,
    pub target: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
}

/// A nested target executable locates configuration through artifacts.toml.
/// A malformed matching deployment must not fall back to checkout credentials.
pub fn packaged_config(directory: &Path, application: Option<&str>) -> Result<Option<PathBuf>> {
    let adjacent = directory.join("config.toml");
    if adjacent.try_exists()? {
        return Ok(Some(adjacent));
    }
    for ancestor in directory.ancestors() {
        let inventory = ancestor.join("artifacts.toml");
        let text = match std::fs::read_to_string(&inventory) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("Cannot read artifacts.toml"),
        };
        let artifacts: Artifacts =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("Invalid artifacts.toml schema"))?;
        ensure!(artifacts.version == 1, "Unsupported artifacts.toml version");
        if application.is_none_or(|application| artifacts.application == application) {
            let config = ancestor.join("config.toml");
            ensure!(config.is_file(), "Packaged config.toml is missing");
            return Ok(Some(config));
        }
    }
    Ok(None)
}

/// Packaged config takes precedence over checkout discovery.
pub fn application_config(application: &str) -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    if let Some(packaged) = packaged_config(
        executable
            .parent()
            .context("Executable directory missing")?,
        Some(application),
    )? {
        return Ok(packaged);
    }
    application_development_config(application)?.with_context(|| {
        format!("No {application} configuration found. Set up the checkout's .snap/development/config.toml, or use --config PATH")
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config<T> {
    pub version: u32,
    pub host: Host,
    pub app: T,
    pub dev: Option<Development>,
    #[serde(skip)]
    root: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Development {
    pub listen: SocketAddr,
}
impl<T: DeserializeOwned> Config<T> {
    /// Config-relative resource paths do not depend on the process working directory.
    pub fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize().context("Cannot open config.toml")?;
        let text = std::fs::read_to_string(&path).context("Cannot read config.toml")?;
        // Parser diagnostics can contain source lines. Do not echo configuration.
        let mut config: Self =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("Invalid config.toml schema"))?;
        ensure!(config.version == 1, "Unsupported config.toml version");
        config.root = path
            .parent()
            .context("Config directory missing")?
            .to_owned();
        config.host.validate()?;
        if let Some(origin) = &config.host.origin {
            config.host.origin = Some(url::Url::parse(origin)?.origin().ascii_serialization());
        }
        ensure!(
            config.host.mode != Mode::Production || config.dev.is_none(),
            "Production forbids development configuration"
        );
        Ok(config)
    }
    pub fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root.join(path)
    }
    pub fn database(&self) -> PathBuf {
        self.path(&self.host.data_dir).join(&self.host.database)
    }
    pub fn assets(&self) -> PathBuf {
        self.path(&self.host.web_dir)
    }
    /// Packaging relocates relative paths. Reject storage under the replaceable
    /// dist tree, including paths routed there through existing symlinks.
    pub fn validate_package_data(&self, package: &Path) -> Result<()> {
        // Publication replaces this node. Following its old symlink would validate
        // a different layout than the real directory that will be installed.
        match std::fs::symlink_metadata(package) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Package output must not be a symlink"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot inspect package output"),
        }
        let dist = resolved_path(package.parent().context("Missing output directory")?)?;
        for path in [
            package.join(&self.host.data_dir),
            package.join(&self.host.data_dir).join(&self.host.database),
        ] {
            ensure!(
                !resolved_path(&path)?.starts_with(&dist),
                "Persistent data must be outside dist"
            );
        }
        Ok(())
    }
    pub fn require_bag(&self, required: bool) -> Result<()> {
        if required {
            ensure!(
                self.root.join("secrets.enc").is_file(),
                "Required secrets.enc is missing; initialize and seal the deployment bag"
            );
        }
        Ok(())
    }
    /// A missing bag is allowed for secretless applications. Required references
    /// still fail when resolved. Present bags always require a valid master key.
    pub fn secrets(&self, key: Option<&MasterKey>) -> Result<Secrets> {
        let path = self.root.join("secrets.enc");
        match std::fs::read(path) {
            Ok(bytes) => Secrets::decrypt(&bytes, key.context("SNAP_MASTER_KEY is required")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Secrets::default()),
            Err(_) => bail!("Cannot read secrets.enc"),
        }
    }
    /// Explicit environment keys always win, including when invalid. Development
    /// may otherwise read a private config-adjacent secrets.key; production never does.
    /// The supervisor and native hosts use the same selection and privacy checks.
    pub fn master_key(&self) -> Result<Option<MasterKey>> {
        match std::env::var("SNAP_MASTER_KEY") {
            Ok(value) => {
                return value
                    .trim()
                    .parse()
                    .map(Some)
                    .context("Invalid SNAP_MASTER_KEY");
            }
            Err(std::env::VarError::NotUnicode(_)) => bail!("Invalid SNAP_MASTER_KEY"),
            Err(std::env::VarError::NotPresent) => {}
        }
        if self.host.mode != Mode::Development {
            return Ok(None);
        }
        match MasterKey::read(&self.path("secrets.key")) {
            Ok(key) => Ok(Some(key)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    pub fn load_secrets(&self) -> Result<Secrets> {
        let key = self.master_key()?;
        self.secrets(key.as_ref())
    }
}
fn resolved_path(path: &Path) -> Result<PathBuf> {
    use std::path::Component;
    let absolute = std::env::current_dir()?.join(path);
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            component => {
                resolved.push(component.as_os_str());
                match std::fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        resolved = resolved
                            .canonicalize()
                            .context("Cannot resolve storage path")?
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).context("Cannot inspect storage path"),
                }
            }
        }
    }
    Ok(resolved)
}
impl Host {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.data_dir.as_os_str().is_empty(),
            "host.data_dir is required"
        );
        ensure!(
            self.database.components().count() == 1
                && matches!(
                    self.database.components().next(),
                    Some(std::path::Component::Normal(_))
                ),
            "host.database must be a filename"
        );
        if self.mode == Mode::Development {
            ensure!(
                self.listen.ip().is_loopback(),
                "Development server must bind loopback"
            );
        } else {
            ensure!(
                self.dev_origins.is_empty() && self.dev_client_origins.is_empty(),
                "Production forbids development origins"
            );
            ensure!(
                self.data_dir.is_absolute(),
                "Production host.data_dir must be absolute"
            );
            ensure!(
                self.origin
                    .as_deref()
                    .is_some_and(|s| s.starts_with("https://")),
                "Production requires an HTTPS host.origin"
            );
        }
        if let Some(origin) = &self.origin {
            validate_origin(origin)?;
        }
        for origin in self
            .dev_origins
            .iter()
            .chain(self.dev_client_origins.values().flatten())
        {
            validate_origin(origin)?;
        }
        Ok(())
    }
    pub fn public_origin(&self, actual: SocketAddr) -> String {
        self.origin
            .clone()
            .unwrap_or_else(|| format!("http://{actual}"))
    }
}
pub fn validate_origin(origin: &str) -> Result<()> {
    let url = url::Url::parse(origin).map_err(|_| anyhow::anyhow!("Invalid configured origin"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "Configured URL must be an HTTP(S) origin"
    );
    Ok(())
}

/// The same arguments are accepted by all native servers. Schema checking and
/// migration never decrypt secrets or open a listener.
pub struct Options {
    pub config: PathBuf,
    pub action: Action,
}
#[derive(PartialEq, Eq)]
pub enum Action {
    Serve,
    Check,
    Migrate,
}
impl Options {
    pub fn parse() -> Result<Self> {
        Self::from_args(std::env::args().skip(1))
    }
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let directory = executable
            .parent()
            .context("Executable directory missing")?;
        let mut config = directory.join("config.toml");
        let mut explicit_config = false;
        let mut action = Action::Serve;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    config = args.next().context("--config requires a path")?.into();
                    explicit_config = true;
                }
                "--check-config" if action == Action::Serve => action = Action::Check,
                "--migrate" if action == Action::Serve => action = Action::Migrate,
                _ => bail!("usage: server [--config PATH] [--check-config | --migrate]"),
            }
        }
        if !explicit_config && let Some(packaged) = packaged_config(directory, None)? {
            config = packaged;
        }
        Ok(Self { config, action })
    }
}
