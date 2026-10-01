//! Native fixture ownership. Children have independent process groups; Drop kills
//! and reaps them before fixture directories can be removed, including cancellation.
use anyhow::{Context, Result, ensure};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};

static ARTIFACTS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub fn set_artifacts(path: PathBuf) {
    let _ = ARTIFACTS.set(path);
}
pub fn artifacts() -> Option<&'static PathBuf> {
    ARTIFACTS.get()
}

/// Owns an async fixture task. Cancellation aborts it rather than detaching a
/// listener; ordinary completion can wait for cancellation to release resources.
pub struct Task(Option<tokio::task::JoinHandle<()>>);
impl Task {
    pub fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(handle))
    }
    pub async fn stop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

pub fn root() -> PathBuf {
    std::env::var_os("SNAP_BROWSER_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .unwrap()
        })
}
pub fn scratch(prefix: &str) -> Result<tempfile::TempDir> {
    let directory = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/coding-agents")
        });
    std::fs::create_dir_all(&directory)?;
    Ok(tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(directory)?)
}
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.env_remove("SNAP_MASTER_KEY");
    c
}
pub fn run(cmd: &mut Command) -> Result<String> {
    let description = describe(cmd);
    let output = cmd
        .output()
        .with_context(|| format!("spawning {description}"))?;
    ensure!(
        output.status.success(),
        "{description} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub async fn checked(command: &mut Command, seconds: u64) -> Result<()> {
    let description = describe(command);
    let mut process = Process::start(command)?;
    let status = timeout(Duration::from_secs(seconds), async {
        loop {
            if let Some(status) = process.status()? {
                return Ok::<_, anyhow::Error>(status);
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .with_context(|| format!("{description} timed out"))??;
    ensure!(status.success(), "{description} failed: {}", process.log());
    process.stop()?;
    Ok(())
}

/// A production TypeScript bundle served by a scoped Rust listener. Bun only
/// compiles the entrypoint; no JavaScript test runner or assertions are involved.
pub struct BundleHost {
    pub base: String,
    _server: Task,
}
impl BundleHost {
    pub async fn start(entry: &Path) -> Result<Self> {
        let scratch = scratch("snap-client-bundle-")?;
        let outdir = scratch.path().join("out");
        let output = outdir
            .join(entry.file_stem().context("bundle entry filename")?)
            .with_extension("js");
        let mut command = command("bun");
        command
            .args([
                "build",
                "--target=browser",
                "--define",
                "process.env.NODE_ENV:\"production\"",
                "--outdir",
            ])
            .arg(&outdir)
            .arg(entry);
        let mut build = Process::start(&mut command)?;
        let status = timeout(Duration::from_secs(60), async {
            loop {
                if let Some(status) = build.status()? {
                    return Ok::<_, anyhow::Error>(status);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("frontend bundle timed out")??;
        ensure!(status.success(), "frontend bundle failed: {}", build.log());
        build.stop()?;
        let script = std::fs::read_to_string(output)?;
        let app = axum::Router::new()
            .route(
                "/fixture.js",
                axum::routing::get(move || {
                    let script = script.clone();
                    async move {
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/javascript")],
                            script,
                        )
                    }
                }),
            )
            .fallback(axum::routing::get(|| async {
                axum::response::Html(
                    "<div id='root'></div><script type='module' src='/fixture.js'></script>",
                )
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = Task::new(tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        }));
        Ok(Self {
            base,
            _server: server,
        })
    }
}
fn describe(command: &Command) -> String {
    // Command's Debug formatter includes explicitly supplied environment values,
    // including fixture age keys and agent credentials. Log argv, never env.
    format!(
        "{:?} {:?}",
        command.get_program(),
        command.get_args().collect::<Vec<_>>()
    )
}
pub fn reserve_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// Independent fixture input, not a call into the origin policy under test.
pub fn local_origins(port: u16) -> Result<Vec<String>> {
    let mut hosts = vec!["127.0.0.1".to_owned(), "localhost".to_owned()];
    for interface in nix::ifaddrs::getifaddrs()? {
        if let Some(address) = interface
            .address
            .and_then(|a| a.as_sockaddr_in().map(|a| a.ip()))
            && !address.is_unspecified()
            && !hosts.contains(&address.to_string())
        {
            hosts.push(address.to_string());
        }
    }
    Ok(hosts
        .into_iter()
        .map(|host| format!("http://{host}:{port}"))
        .collect())
}

pub struct SourceCopy {
    pub root: PathBuf,
    directory: tempfile::TempDir,
}
impl SourceCopy {
    pub fn new() -> Result<Self> {
        let directory = scratch("snap-browser-source-")?;
        let destination = directory.path().to_owned();
        let source = root();
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "package.json",
            "bun.lock",
            "tsconfig.json",
            "crates",
            "kits",
            "tools/cli",
            "apps",
            "tests",
        ] {
            copy_tree(&source.join(path), &destination.join(path))?;
        }
        for path in ["node_modules", ".tools", "target"] {
            std::os::unix::fs::symlink(source.join(path), destination.join(path))?;
        }
        Ok(Self {
            root: destination,
            directory,
        })
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            // App-owned JS packages have workspace-local dependency links with
            // Bun's isolated linker. Reuse them just like the root dependencies;
            // copying package sources must not hide server-only dependencies.
            if entry.file_name() == "node_modules" {
                std::os::unix::fs::symlink(entry.path(), destination.join(entry.file_name()))?;
                continue;
            }
            if matches!(
                entry.file_name().to_str(),
                Some(".snap" | ".deployment" | "target" | "build" | "dist" | ".git" | ".tmp")
            ) {
                continue;
            }
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(std::fs::read_link(source)?, destination)?;
    } else {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, destination)?;
    }
    Ok(())
}
pub struct Process {
    child: Child,
    pub logs: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
    stopped: bool,
    identity: Option<u64>,
    retained_descendants: Vec<(u32, u64)>,
}
fn drain(stream: impl Read + Send + 'static, logs: Arc<Mutex<String>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let mut log = logs.lock().unwrap();
            log.push_str(&line);
            log.push('\n');
            if log.len() > 1024 * 1024 {
                let cut = log
                    .char_indices()
                    .find(|(i, _)| *i >= log.len() - 512 * 1024)
                    .unwrap()
                    .0;
                log.drain(..cut);
            }
        }
    })
}
impl Process {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn status(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }
    pub fn start(cmd: &mut Command) -> Result<Self> {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let description = describe(cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {description}"))?;
        let logs = Arc::new(Mutex::new(String::new()));
        let identity = process_identity(child.id()).map(|(_, start)| start);
        let readers = vec![
            drain(child.stdout.take().unwrap(), logs.clone()),
            drain(child.stderr.take().unwrap(), logs.clone()),
        ];
        Ok(Self {
            child,
            logs,
            readers,
            stopped: false,
            identity,
            retained_descendants: Vec::new(),
        })
    }
    pub fn log(&self) -> String {
        self.logs.lock().unwrap().clone()
    }
    pub fn alive(&mut self) -> Result<()> {
        ensure!(
            self.child.try_wait()?.is_none(),
            "host exited: {}",
            self.log()
        );
        Ok(())
    }
    pub async fn ready(&mut self, prefix: &str, endpoint: &str, seconds: u64) -> Result<String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()?;
        timeout(Duration::from_secs(seconds), async {
            loop {
                self.alive()?;
                let url = self.log().lines().find_map(|l| {
                    l.strip_prefix(prefix)
                        .and_then(|s| s.split_whitespace().next())
                        .map(str::to_owned)
                });
                if let Some(url) = url
                    && client
                        .get(format!("{url}{endpoint}"))
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success())
                {
                    return Ok(url);
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .with_context(|| format!("host readiness timed out: {}", self.log()))?
    }
    /// Signals only the supervisor process and observes its exit, without killing
    /// separately grouped descendants. Lifecycle journeys must observe listener
    /// closure before `stop` supplies the unconditional fixture cleanup.
    pub async fn terminate(&mut self) -> Result<std::process::ExitStatus> {
        self.capture_descendants();
        if process_identity(self.child.id()).is_none_or(|(_, start)| Some(start) == self.identity) {
            nix::sys::signal::kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM)?;
        }
        timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Ok(status);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .with_context(|| format!("supervisor did not exit after SIGTERM: {}", self.log()))?
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.capture_descendants();
        let descendants = std::mem::take(&mut self.retained_descendants);
        // Development supervisors own additional child groups. Give their real
        // shutdown path a chance to close those before escalating the outer group.
        let same_process =
            |pid| process_identity(pid).is_none_or(|(_, start)| Some(start) == self.identity);
        if same_process(self.child.id()) {
            let _ = killpg(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if same_process(self.child.id()) {
            let _ = killpg(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        }
        // The supervisor may put Vite/backend children in their own groups.
        // Kill only captured descendants whose /proc start time still matches,
        // never a PID that has since been recycled by an unrelated process.
        for (pid, start) in descendants {
            if process_identity(pid).is_some_and(|(_, current)| current == start) {
                let _ = nix::sys::signal::kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
            }
        }
        self.child.wait()?;
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        if let Some(path) = artifacts() {
            let _ = std::fs::write(
                path.join(format!("host-{}.log", self.child.id())),
                self.log(),
            );
        }
        self.stopped = true;
        Ok(())
    }
    fn capture_descendants(&mut self) {
        // A graceful wait may already have reaped the supervisor. Never attach
        // an unrelated process tree if the kernel has recycled its PID.
        if process_identity(self.child.id()).is_some_and(|(_, start)| Some(start) == self.identity)
        {
            self.retained_descendants
                .extend(descendants(self.child.id()));
        }
    }
    pub async fn wait_log(&mut self, needle: &str, seconds: u64) -> Result<()> {
        timeout(Duration::from_secs(seconds), async {
            loop {
                self.alive()?;
                if self.log().contains(needle) {
                    return Ok(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .with_context(|| format!("missing host log {needle}: {}", self.log()))?
    }
}

fn process_identity(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}
fn descendants(parent: u32) -> Vec<(u32, u64)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let processes = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let (ppid, start) = process_identity(pid)?;
            Some((pid, ppid, start))
        })
        .collect::<Vec<_>>();
    let mut owned = vec![parent];
    let mut result = Vec::new();
    loop {
        let before = owned.len();
        for &(pid, ppid, start) in &processes {
            if owned.contains(&ppid) && !owned.contains(&pid) {
                owned.push(pid);
                result.push((pid, start));
            }
        }
        if owned.len() == before {
            break;
        }
    }
    result
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct Deployment {
    pub path: PathBuf,
    pub key: Option<String>,
}
impl Deployment {
    pub fn create(directory: &Path, mut config: Value, secrets: Option<Value>) -> Result<Self> {
        let input = directory.join(".deployment/development");
        std::fs::create_dir_all(&input)?;
        std::fs::write(
            directory.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\n",
        )?;
        std::fs::write(
            directory.join("snap.toml"),
            "version=1\napplication='fixture'\n",
        )?;
        config["version"] = serde_json::json!(1);
        let path = input.join("config.toml");
        std::fs::write(&path, toml::to_string(&config)?)?;
        let key = if let Some(secrets) = secrets {
            run(command(root().join("target/debug/snap"))
                .args(["secrets", "init"])
                .current_dir(directory))?;
            use std::os::unix::fs::PermissionsExt;
            let private = input.join("secrets.toml");
            std::fs::write(&private, toml::to_string(&secrets)?)?;
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600))?;
            run(command(root().join("target/debug/snap"))
                .args(["secrets", "seal"])
                .current_dir(directory))?;
            Some(
                std::fs::read_to_string(input.join("secrets.key"))?
                    .trim()
                    .to_owned(),
            )
        } else {
            None
        };
        Ok(Self { path, key })
    }
    pub fn apply<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command.arg("--config").arg(&self.path);
        if let Some(key) = &self.key {
            command.env("SNAP_MASTER_KEY", key);
        } else {
            command.env_remove("SNAP_MASTER_KEY");
        }
        command
    }
}
pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?)
}
pub async fn get(url: &str) -> Result<Value> {
    Ok(client()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
pub async fn post(url: &str, body: Value) -> Result<Value> {
    Ok(client()?
        .post(url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
pub async fn poll<F, Fut>(description: &str, seconds: u64, mut f: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let start = Instant::now();
    timeout(Duration::from_secs(seconds), async {
        loop {
            if f().await? {
                return Ok(());
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for {description} after {:?}", start.elapsed()))?
}
