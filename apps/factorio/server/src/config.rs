#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub oauth: snap_identity_native::oauth::Settings,
    pub repository: factorio::Config,
    #[serde(default)]
    pub tools: Tools,
    #[serde(default)]
    pub tcp: Tcp,
}
#[derive(Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tcp {
    pub listen: std::net::SocketAddr,
    /// Detached logical lifetime retention, independent of login credential expiry.
    pub retention_ms: u64,
    pub cert_file: std::path::PathBuf,
    pub key_file: std::path::PathBuf,
    pub ca_file: Option<std::path::PathBuf>,
    pub server_name: Option<String>,
}
impl Default for Tcp {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:1024".parse().unwrap(),
            retention_ms: 1_800_000,
            cert_file: Default::default(),
            key_file: Default::default(),
            ca_file: None,
            server_name: None,
        }
    }
}
#[derive(Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tools {
    pub bun: String,
    pub opencode: String,
    pub model: String,
    pub bridge: Option<std::path::PathBuf>,
}
impl Default for Tools {
    fn default() -> Self {
        Self {
            bun: "bun".into(),
            opencode: "opencode".into(),
            model: "opencode-go/muse-spark-1.3-contributor".into(),
            bridge: None,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.oauth.validate()?;
        if self.tcp.retention_ms == 0
            || self.tcp.cert_file.as_os_str().is_empty()
            || self.tcp.key_file.as_os_str().is_empty()
        {
            return Err("TCP requires cert_file, key_file and positive retention_ms".into());
        }
        if self.tools.bun.is_empty()
            || self.tools.opencode.is_empty()
            || !self
                .tools
                .model
                .split_once('/')
                .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
        {
            return Err("Invalid tools configuration".into());
        }
        for path in [&self.repository.repository, &self.repository.resources] {
            if !std::path::Path::new(path).is_absolute() {
                return Err("Repository and resources must be absolute paths".into());
            }
        }
        Ok(())
    }
}
