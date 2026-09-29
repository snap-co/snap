#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub oauth: snap_oauth_local::Settings,
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
    pub retention_ms: u64,
}
impl Default for Tcp {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:1248".parse().unwrap(),
            retention_ms: 1_800_000,
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
        if !self.tcp.listen.ip().is_loopback() || self.tcp.retention_ms == 0 {
            return Err("TCP requires loopback and positive retention_ms; use an SSH tunnel for remote access".into());
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
