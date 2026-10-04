//! Private, process-shared connection identity and invocation allocation. The
//! file stays locked through detach; IDs are durably reserved before network IO.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
// CBOR strings may expand sixfold into JSON escapes. Keep exact pending input
// recoverable at the logical wire bound, but reject corrupt/unbounded state files.
const STATE_LIMIT: usize = snap_transport::binary::LOGICAL_MESSAGE_LIMIT * 6 + 65536;

fn client_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn first() -> u64 {
    1
}
#[derive(Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default, alias = "token")]
    pub bearer: String,
    #[serde(default = "client_id")]
    pub client_id: String,
    #[serde(default = "first")]
    pub next_id: u64,
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub expires: Option<i64>,
    #[serde(default)]
    pub pending: Option<snap_transport::Invocation>,
    #[serde(default)]
    pub lifetime: Option<String>,
}
pub struct Locked {
    file: Option<File>,
    pub value: Credentials,
}
impl Locked {
    pub fn open(path: &Path, seed: Option<&str>) -> Result<Self> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(seed.is_some())
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("Cannot open {}. Run factorio login first", path.display()))?;
        file.lock()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata()?;
            ensure!(
                meta.mode() & 0o077 == 0,
                "Credential file must be private. Run chmod 600 {}",
                path.display()
            );
            ensure!(
                meta.nlink() == 1,
                "Credential file must not have hard links"
            );
        }
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() <= STATE_LIMIT as u64,
            "Invalid credential file"
        );
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let value = if bytes.is_empty()
            && let Some(seed) = seed
        {
            Credentials {
                bearer: seed.into(),
                client_id: client_id(),
                next_id: 1,
                addr: None,
                ca_file: None,
                server_name: None,
                workspace: String::new(),
                expires: None,
                pending: None,
                lifetime: None,
            }
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("Invalid credential file"))?
        };
        let mut locked = Self {
            file: Some(file),
            value,
        };
        locked.save()?;
        Ok(locked)
    }
    pub fn save(&mut self) -> Result<()> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&self.value)?;
        ensure!(
            bytes.len() <= STATE_LIMIT,
            "Credential/recovery state exceeds the bounded file limit"
        );
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&bytes)?;
        file.set_len(bytes.len() as u64)?;
        file.sync_all()?;
        Ok(())
    }
    /// A long-lived watcher must not occupy the command file lock or its logical
    /// attachment. It owns an independent ephemeral identity and counter instead.
    pub fn isolated(mut self) -> Self {
        self.file.take();
        self.value.client_id = client_id();
        self.value.next_id = 1;
        self.value.pending = None;
        self.value.lifetime = None;
        self
    }
    pub fn reserve(&mut self) -> Result<u64> {
        let id = self.value.next_id;
        ensure!(id > 0, "Invalid invocation counter");
        self.value.next_id = id.checked_add(1).context("Invocation IDs exhausted")?;
        self.save()?;
        Ok(id)
    }
}
pub fn default_path(addr: &str, token: Option<&str>) -> Result<PathBuf> {
    // Keep the existing directory and endpoint digest across the factory -> factorio
    // executable rename so login, counters and uncertain outcomes survive upgrades.
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("Set HOME or --credentials")?
        .join("factory");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{addr}\n{}", token.unwrap_or("")).as_bytes())
    );
    Ok(root.join(format!("{digest}.json")))
}
