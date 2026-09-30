//! Real native Authy and Chatty process fixtures for app browser journeys.
//!
//! Both hosts run the production binaries against disposable data directories
//! with explicitly migrated databases and ephemeral loopback listeners. The
//! configuration registers the `chatty` client with Authy
//! (plus `factorio` when a factorio secret override is supplied), Chatty
//! resolves its OAuth issuer to the paired Authy base, and restart preserves
//! the database and base URL by rebinding the same reserved listener.
//!
//! There is no JavaScript control plane: journeys call `restart` directly on
//! the fixture instead of hitting a `/restart` HTTP endpoint. `Process` is
//! always declared before its data directory so dropping a fixture kills and
//! reaps the child before the directory can be removed.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::support::{self, Deployment, Process};

/// Client secret shared by the fixture Authy issuer and the Chatty relying
/// party. It satisfies the native hosts' minimum 32-byte confidential-client
/// requirement.
pub const CLIENT_SECRET: &str = "fixture-client-secret-with-at-least-32-bytes";

fn override_string(overrides: &Value, key: &str) -> Option<String> {
    overrides
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// Parses the `SNAP_DEV_CLIENT_ORIGINS` override: an object mapping client IDs
/// to origin lists; an already-decoded array is wrapped as the `chatty`
/// client's origins and a bare string becomes its single origin.
fn dev_client_origins(overrides: &Value) -> Result<Option<Value>> {
    let raw = match overrides.get("SNAP_DEV_CLIENT_ORIGINS") {
        None => return Ok(None),
        Some(raw) => raw,
    };
    if let Some(object) = raw.as_object() {
        return Ok(Some(Value::Object(object.clone())));
    }
    if let Some(items) = raw.as_array() {
        return Ok(Some(json!({ "chatty": items })));
    }
    if let Some(text) = raw.as_str() {
        let parsed: Value =
            serde_json::from_str(text).context("invalid SNAP_DEV_CLIENT_ORIGINS")?;
        if let Some(object) = parsed.as_object() {
            return Ok(Some(Value::Object(object.clone())));
        }
        if let Some(items) = parsed.as_array() {
            return Ok(Some(json!({ "chatty": items })));
        }
        return Ok(Some(json!({ "chatty": [parsed] })));
    }
    anyhow::bail!("invalid SNAP_DEV_CLIENT_ORIGINS");
}

fn migrate(binary: &Path, deployment: &Deployment) -> Result<()> {
    let mut command = support::command(binary);
    command.current_dir(support::root());
    deployment.apply(&mut command).arg("--migrate");
    support::run(&mut command)?;
    Ok(())
}

fn spawn(binary: &Path, deployment: &Deployment) -> Result<Process> {
    let mut command = support::command(binary);
    command.current_dir(support::root());
    deployment.apply(&mut command);
    Process::start(&mut command)
}

/// Real native Authy host with an explicitly migrated disposable database.
///
/// The listener port is reserved before startup so `restart` rebinds the same
/// base URL against the preserved database.
pub struct AuthyHost {
    pub base: String,
    pub client_secret: String,
    pub process: Process,
    directory: tempfile::TempDir,
    deployment: Deployment,
    binary: PathBuf,
}

impl AuthyHost {
    pub fn database(&self) -> PathBuf {
        self.directory.path().join("authy.sqlite")
    }
    /// Starts Authy with `rp` registered as the `chatty` client origin.
    /// `overrides` accepts fixture configuration keys:
    /// `FACTORIO_ORIGIN`, `FACTORIO_CLIENT_SECRET`, `SNAP_DEV_CLIENT_ORIGINS`
    /// (object, array, bare string, or JSON-encoded string),
    /// `AUTHY_AUTO_APPROVE_DOMAIN` (empty disables auto-approval),
    /// `AUTHY_APP_DOMAIN`, and `SNAP_ORIGIN` (public host origin).
    pub async fn start(rp: &str, overrides: Value) -> Result<Self> {
        let root = support::root();
        let web_dir = root.join("apps/authy/dist/development/web");
        ensure!(
            web_dir.join("index.html").is_file(),
            "missing Authy web assets: {}",
            web_dir.display()
        );
        let port = support::reserve_port()?;
        let directory = support::scratch("snap-authy-host-")?;
        let mut clients = vec![json!({
            "id": "chatty",
            "name": "Chatty",
            "origin": rp,
            "client_secret_ref": "clients.chatty",
        })];
        let mut secrets = json!({ "clients": { "chatty": CLIENT_SECRET } });
        if let Some(secret) = override_string(&overrides, "FACTORIO_CLIENT_SECRET")
            && !secret.is_empty()
        {
            let origin = override_string(&overrides, "FACTORIO_ORIGIN")
                .context("FACTORIO_ORIGIN is required with FACTORIO_CLIENT_SECRET")?;
            clients.push(json!({
                "id": "factorio",
                "name": "Factorio",
                "origin": origin,
                "client_secret_ref": "clients.factorio",
            }));
            secrets["clients"]["factorio"] = json!(secret);
        }
        let mut host = json!({
            "mode": "development",
            "listen": format!("127.0.0.1:{port}"),
            "data_dir": directory.path().to_string_lossy(),
            "database": "authy.sqlite",
            "web_dir": web_dir.to_string_lossy(),
        });
        if let Some(origin) = override_string(&overrides, "SNAP_ORIGIN") {
            host["origin"] = json!(origin);
        }
        if let Some(origins) = dev_client_origins(&overrides)? {
            host["dev_client_origins"] = origins;
        }
        let mut app = json!({
            "clients": clients,
            "auto_approve_domain": override_string(&overrides, "AUTHY_AUTO_APPROVE_DOMAIN")
                .unwrap_or_else(|| "snapco.dev".to_owned()),
        });
        if let Some(domain) = override_string(&overrides, "AUTHY_APP_DOMAIN") {
            app["app_domain"] = json!(domain);
        }
        let deployment = Deployment::create(
            directory.path(),
            json!({ "host": host, "app": app }),
            Some(secrets),
        )?;
        let binary = root.join("target/debug/authy");
        migrate(&binary, &deployment)?;
        let mut process = spawn(&binary, &deployment)?;
        let base = process.ready("Authy ", "/health", 20).await?;
        Ok(Self {
            base,
            client_secret: CLIENT_SECRET.to_owned(),
            process,
            directory,
            deployment,
            binary,
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        self.process.stop()
    }

    /// Fixture data directory, retained for the host lifetime.
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Restarts the host against the preserved database and base URL.
    pub async fn restart(&mut self) -> Result<()> {
        self.process.stop()?;
        self.process = spawn(&self.binary, &self.deployment)?;
        let base = self.process.ready("Authy ", "/health", 20).await?;
        ensure!(
            base == self.base,
            "authy restart changed base URL: {base} != {}",
            self.base
        );
        Ok(())
    }
}

/// Real native Chatty host paired with a fixture Authy issuer.
///
/// `stop` halts only the Chatty child, mirroring `pair.stop`; dropping the
/// fixture stops the Chatty child before the Authy host (field order), then
/// removes either data directory, mirroring `pair.close` without the Bun
/// control plane.
pub struct ChattyHost {
    pub base: String,
    pub process: Process,
    pub authy: AuthyHost,
    directory: tempfile::TempDir,
    deployment: Deployment,
    dev: bool,
    source_root: PathBuf,
}

fn spawn_chatty(dev: bool, source_root: &Path, deployment: &Deployment) -> Result<Process> {
    if dev {
        let mut command = support::command(source_root.join("target/debug/snap"));
        command.arg("dev").arg(source_root.join("apps/chatty"));
        deployment.apply(&mut command);
        command.current_dir(source_root);
        Process::start(&mut command)
    } else {
        spawn(&support::root().join("target/debug/chatty"), deployment)
    }
}

async fn await_dev_ready(process: &mut Process, base: &str) -> Result<()> {
    process.wait_log("Chatty dev http", 20).await?;
    let client = support::client()?;
    let url = format!("{base}/api/session");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            process.alive()?;
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .with_context(|| format!("chatty dev readiness timed out: {}", process.log()))?
}

impl ChattyHost {
    /// Starts a Chatty/Authy pair. Chatty's web assets always come from the
    /// checkout root's built `dist` (a dev `SourceCopy` excludes `dist`, so
    /// it cannot supply them); `source_root` selects the supervised project
    /// directory for `dev` mode and is otherwise informational. Authy always
    /// serves the checkout root's assets. With `dev`, Chatty runs under the
    /// `snap dev` supervisor from `source_root`; otherwise the `chatty`
    /// binary serves directly. Migration always uses the binary.
    pub async fn start(source_root: &Path, dev: bool) -> Result<Self> {
        let port = support::reserve_port()?;
        let base = format!("http://127.0.0.1:{port}");
        let authy = if dev {
            let origins = support::local_origins(port)?;
            AuthyHost::start(
                &base,
                json!({ "SNAP_DEV_CLIENT_ORIGINS": serde_json::to_string(&json!({ "chatty": origins }))? }),
            )
            .await?
        } else {
            AuthyHost::start(&base, json!({})).await?
        };
        let directory = support::scratch("snap-chatty-pair-")?;
        let web_dir = support::root().join("apps/chatty/dist/development/web");
        ensure!(
            web_dir.join("index.html").is_file(),
            "missing Chatty web assets: {}",
            web_dir.display()
        );
        let mut config = json!({
            "host": {
                "mode": "development",
                "listen": format!("127.0.0.1:{port}"),
                "origin": base,
                "data_dir": directory.path().to_string_lossy(),
                "database": "chatty.sqlite",
                "web_dir": web_dir.to_string_lossy(),
            },
            "app": {
                "oauth": {
                    "issuer": authy.base,
                    "client_id": "chatty",
                    "client_secret_ref": "oauth.client_secret",
                },
            },
        });
        if dev {
            config["dev"] = json!({ "listen": format!("0.0.0.0:{port}") });
        }
        let deployment = Deployment::create(
            directory.path(),
            config,
            Some(json!({ "oauth": { "client_secret": authy.client_secret } })),
        )?;
        let binary = support::root().join("target/debug/chatty");
        migrate(&binary, &deployment)?;
        let mut process = spawn_chatty(dev, source_root, &deployment)?;
        if dev {
            await_dev_ready(&mut process, &base).await?;
        } else {
            let actual = process.ready("Chatty ", "/health", 20).await?;
            ensure!(
                actual == base,
                "chatty restart changed base URL: {actual} != {base}"
            );
        }
        Ok(Self {
            base,
            process,
            authy,
            directory,
            deployment,
            dev,
            source_root: source_root.to_owned(),
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        self.process.stop()
    }

    /// Fixture data directory, retained for the host lifetime.
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Restarts Chatty against the preserved database and base URL. The paired
    /// Authy host keeps running, so OAuth leases and sessions survive.
    pub async fn restart(&mut self) -> Result<()> {
        self.process.stop()?;
        self.process = spawn_chatty(self.dev, &self.source_root, &self.deployment)?;
        if self.dev {
            await_dev_ready(&mut self.process, &self.base).await?;
        } else {
            let actual = self.process.ready("Chatty ", "/health", 20).await?;
            ensure!(
                actual == self.base,
                "chatty restart changed base URL: {actual} != {}",
                self.base
            );
        }
        Ok(())
    }
}
