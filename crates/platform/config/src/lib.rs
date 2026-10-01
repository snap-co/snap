//! Host-owned startup configuration. Portable modules receive resolved inputs,
//! never this loader, filesystem paths to secrets, or ambient configuration.
use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, de::DeserializeOwned};
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

/// Packaged config beside the executable takes precedence over checkout discovery.
pub fn application_config(application: &str) -> Result<PathBuf> {
    let packaged = std::env::current_exe()?.with_file_name("config.toml");
    if packaged.try_exists()? {
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
    /// still fail when resolved. Present bags always require a valid identity.
    pub fn secrets(&self, identity: Option<&age::x25519::Identity>) -> Result<Secrets> {
        let path = self.root.join("secrets.enc");
        match std::fs::read(path) {
            Ok(bytes) => Secrets::decrypt(&bytes, identity.context("SNAP_MASTER_KEY is required")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Secrets::default()),
            Err(_) => bail!("Cannot read secrets.enc"),
        }
    }
    /// Explicit environment keys always win, including when invalid. Development
    /// may otherwise read a private config-adjacent secrets.key; production never does.
    /// The supervisor and native hosts use the same selection and privacy checks.
    pub fn master_key(&self) -> Result<Option<age::x25519::Identity>> {
        match std::env::var("SNAP_MASTER_KEY") {
            Ok(value) => {
                return value
                    .trim()
                    .parse()
                    .map(Some)
                    .map_err(|_| anyhow::anyhow!("Invalid SNAP_MASTER_KEY"));
            }
            Err(std::env::VarError::NotUnicode(_)) => bail!("Invalid SNAP_MASTER_KEY"),
            Err(std::env::VarError::NotPresent) => {}
        }
        if self.host.mode != Mode::Development {
            return Ok(None);
        }
        let mut file = match std::fs::File::open(self.path("secrets.key")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Cannot open development secrets.key"),
        };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= 4096,
            "Invalid development secrets.key file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "Development secrets.key must be private; use chmod 600"
            );
        }
        use std::io::Read;
        let mut value = String::new();
        file.read_to_string(&mut value)
            .context("Cannot read development secrets.key")?;
        value
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| anyhow::anyhow!("Invalid development secrets.key"))
    }
    pub fn load_secrets(&self) -> Result<Secrets> {
        let identity = self.master_key()?;
        self.secrets(identity.as_ref())
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

#[derive(Clone)]
pub struct SecretRef(String);
impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        if name.split('.').any(str::is_empty)
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        {
            return Err(serde::de::Error::custom("Invalid secret reference"));
        }
        Ok(Self(name))
    }
}
impl SecretRef {
    pub fn name(&self) -> &str {
        &self.0
    }
}
pub struct Secret(SecretString);
impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}
impl Secret {
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}
#[derive(Default)]
pub struct Secrets(BTreeMap<String, Secret>);
impl Secrets {
    pub fn resolve(&self, reference: &SecretRef) -> Result<&Secret> {
        self.0
            .get(reference.name())
            .with_context(|| format!("Missing secret: {}", reference.name()))
    }
    pub fn encrypt(plaintext: &[u8], recipients: &[age::x25519::Recipient]) -> Result<Vec<u8>> {
        use std::io::Write;
        // Validate the bag before encrypting. No plaintext appears in diagnostics.
        Self::parse(plaintext)?;
        let encryptor =
            age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient))
                .context("No age recipients")?;
        let mut output = Vec::new();
        let mut writer = encryptor.wrap_output(&mut output)?;
        writer.write_all(plaintext)?;
        writer.finish()?;
        Ok(output)
    }
    pub fn decrypt(ciphertext: &[u8], identity: &age::x25519::Identity) -> Result<Self> {
        let plaintext = age::decrypt(identity, ciphertext)
            .map_err(|_| anyhow::anyhow!("Secrets decryption failed"))?;
        Self::parse(&plaintext)
    }
    fn parse(bytes: &[u8]) -> Result<Self> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("Invalid secrets bag"))?;
        let table: toml::Table =
            toml::from_str(text).map_err(|_| anyhow::anyhow!("Invalid secrets bag"))?;
        fn collect(
            prefix: &str,
            table: toml::Table,
            output: &mut BTreeMap<String, Secret>,
        ) -> Result<()> {
            for (key, value) in table {
                ensure!(
                    !key.is_empty() && !key.contains('.'),
                    "Secret keys must be nonempty without dots"
                );
                let name = if prefix.is_empty() {
                    key
                } else {
                    format!("{prefix}.{key}")
                };
                match value {
                    toml::Value::String(s) => {
                        output.insert(name, Secret(s.into()));
                    }
                    toml::Value::Table(t) => collect(&name, t, output)?,
                    _ => bail!("Secrets must be strings or nested tables"),
                }
            }
            Ok(())
        }
        let mut output = BTreeMap::new();
        collect("", table, &mut output)?;
        Ok(Self(output))
    }
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
        let mut config = std::env::current_exe()?.with_file_name("config.toml");
        let mut action = Action::Serve;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => config = args.next().context("--config requires a path")?.into(),
                "--check-config" if action == Action::Serve => action = Action::Check,
                "--migrate" if action == Action::Serve => action = Action::Migrate,
                _ => bail!("usage: server [--config PATH] [--check-config | --migrate]"),
            }
        }
        Ok(Self { config, action })
    }
}
