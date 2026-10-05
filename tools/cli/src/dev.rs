//! One owner for development configuration, builds, publication and child lifetime.
//! Vite is an adapter; it never starts a Rust process or reads deployment secrets.
use crate::{
    build, cargo,
    config::Project,
    process::{OwnedProcess, Runner},
};
use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, bail, ensure};
use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{ChildStdout, Command},
    sync::mpsc,
};

#[derive(Clone)]
struct Installation {
    document: toml::Table,
    key: Option<String>,
    bag: Option<Vec<u8>>,
    listen: SocketAddr,
    origin: String,
    origins: Vec<String>,
}
impl Installation {
    async fn load(
        path: &Path,
        runner: &Runner,
        previous: Option<SocketAddr>,
        application: &str,
    ) -> Result<Self> {
        let config = snap_config::Config::<toml::Table>::read(path)?;
        ensure!(
            config.host.mode == snap_config::Mode::Development,
            "snap dev requires development configuration"
        );
        let mut listen = config
            .dev
            .as_ref()
            .map_or(config.host.listen, |dev| dev.listen);
        // Testy's debugger is trusted control IO, unlike the other apps' LAN UI.
        ensure!(
            application != "testy" || listen.ip().is_loopback(),
            "Testy development requires a loopback frontend"
        );
        let tls = config
            .host
            .origin
            .as_deref()
            .is_some_and(|origin| origin.starts_with("https:"));
        ensure!(
            !tls || listen.ip().is_loopback(),
            "HTTPS development requires a loopback frontend"
        );
        if listen.port() == 0 {
            listen.set_port(match previous.filter(|old| old.ip() == listen.ip()) {
                Some(old) => old.port(),
                None => TcpListener::bind(listen)?.local_addr()?.port(),
            });
        }
        let public = SocketAddr::new(
            if listen.ip().is_unspecified() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                listen.ip()
            },
            listen.port(),
        );
        let origin = config.host.public_origin(public);
        let url = url::Url::parse(&origin)?;
        ensure!(
            !matches!(url.host_str(), Some("0.0.0.0" | "[::]")),
            "Public origin must name a reachable host"
        );
        let mut hosts = local_hosts(runner).await?;
        hosts.insert(
            url.host_str()
                .context("Missing public hostname")?
                .to_owned(),
        );
        let origins = if tls {
            vec![origin.clone()]
        } else {
            let mut origins: BTreeSet<_> = hosts
                .iter()
                .map(|host| http_origin(host, listen.port()))
                .collect();
            origins.insert(origin.clone());
            origins.into_iter().collect()
        };
        let mut document: toml::Table = toml::from_str(&std::fs::read_to_string(path)?)
            .map_err(|_| anyhow::anyhow!("Invalid config.toml schema"))?;
        document.remove("dev");
        let host = document
            .get_mut("host")
            .and_then(toml::Value::as_table_mut)
            .context("Missing host")?;
        host.insert(
            "data_dir".into(),
            config
                .path(&config.host.data_dir)
                .to_string_lossy()
                .into_owned()
                .into(),
        );
        // App-owned bridge paths retain the original config-relative meaning.
        if let Some(bridge) = document
            .get_mut("app")
            .and_then(toml::Value::as_table_mut)
            .and_then(|app| app.get_mut("tools"))
            .and_then(toml::Value::as_table_mut)
            .and_then(|tools| tools.get_mut("bridge"))
            && let Some(value) = bridge.as_str()
        {
            *bridge = config.path(value).to_string_lossy().into_owned().into();
        }
        // TLS files must still resolve against the installation config after
        // the supervisor writes a generation into its disposable directory.
        if let Some(tcp) = document
            .get_mut("app")
            .and_then(toml::Value::as_table_mut)
            .and_then(|app| app.get_mut("tcp"))
            .and_then(toml::Value::as_table_mut)
        {
            for field in ["cert_file", "key_file", "ca_file"] {
                if let Some(path) = tcp.get_mut(field)
                    && let Some(value) = path.as_str()
                {
                    *path = config.path(value).to_string_lossy().into_owned().into();
                }
            }
        }
        let clients = client_origins(&config.app, &hosts)?;
        host_table(&mut document)?
            .insert("dev_client_origins".into(), toml::Value::try_from(clients)?);
        let key = config
            .master_key()?
            .map(|key| key.to_string().expose_secret().to_owned());
        let bag = optional_read(config.path("secrets.enc"))?;
        Ok(Self {
            document,
            key,
            bag,
            listen,
            origin,
            origins,
        })
    }

    fn write_generation(&self, directory: &Path, backend: SocketAddr) -> Result<()> {
        let mut document = self.document.clone();
        let host = host_table(&mut document)?;
        host.insert("listen".into(), backend.to_string().into());
        host.insert("origin".into(), self.origin.clone().into());
        host.insert(
            "web_dir".into(),
            directory.join("web").to_string_lossy().into_owned().into(),
        );
        host.insert("dev_origins".into(), toml::Value::try_from(&self.origins)?);
        std::fs::write(directory.join("config.toml"), toml::to_string(&document)?)?;
        if let Some(bytes) = &self.bag {
            std::fs::write(directory.join("secrets.enc"), bytes)?;
        } else if directory.join("secrets.enc").exists() {
            std::fs::remove_file(directory.join("secrets.enc"))?;
        }
        Ok(())
    }
}
fn host_table(document: &mut toml::Table) -> Result<&mut toml::Table> {
    document
        .get_mut("host")
        .and_then(toml::Value::as_table_mut)
        .context("Missing host")
}
fn optional_read(path: PathBuf) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn http_origin(host: &str, port: u16) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    if port == 80 {
        format!("http://{host}")
    } else {
        format!("http://{host}:{port}")
    }
}
async fn local_hosts(runner: &Runner) -> Result<BTreeSet<String>> {
    let mut hosts = BTreeSet::from(["127.0.0.1".to_owned(), "localhost".to_owned()]);
    for interface in nix::ifaddrs::getifaddrs()? {
        if let Some(ip) = interface
            .address
            .and_then(|address| address.as_sockaddr_in().map(|address| address.ip()))
            && !ip.is_unspecified()
        {
            hosts.insert(ip.to_string());
        }
    }
    let mut tailscale = Command::new("tailscale");
    tailscale
        .args(["status", "--json"])
        .stderr(Stdio::null())
        .env_remove("SNAP_MASTER_KEY");
    if let Ok(Ok(bytes)) =
        tokio::time::timeout(Duration::from_secs(2), runner.run(&mut tailscale, true)).await
        && let Ok(status) = serde_json::from_slice::<Value>(&bytes)
        && let Some(name) = status["Self"]["DNSName"].as_str()
    {
        let name = name.trim_end_matches('.');
        for candidate in [name, name.split('.').next().unwrap_or(name)] {
            if let Ok(Ok(addresses)) = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::lookup_host((candidate, 80)),
            )
            .await
                && addresses
                    .into_iter()
                    .any(|address| hosts.contains(&address.ip().to_string()))
            {
                hosts.insert(candidate.to_owned());
            }
        }
    }
    runner.check()?;
    Ok(hosts)
}
fn client_origins(app: &toml::Table, hosts: &BTreeSet<String>) -> Result<toml::Table> {
    let mut result = toml::Table::new();
    if app.contains_key("app_domain") {
        return Ok(result);
    }
    for client in app
        .get("clients")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = client.get("id").and_then(toml::Value::as_str) else {
            continue;
        };
        let Some(origin) = client.get("origin").and_then(toml::Value::as_str) else {
            continue;
        };
        snap_config::validate_origin(origin)?;
        let url = url::Url::parse(origin)?;
        let origins = if url.scheme() == "https" {
            vec![url.origin().ascii_serialization()]
        } else {
            let mut origins: BTreeSet<_> = hosts
                .iter()
                .map(|host| http_origin(host, url.port_or_known_default().unwrap_or(80)))
                .collect();
            origins.insert(url.origin().ascii_serialization());
            origins.into_iter().collect()
        };
        result.insert(id.into(), toml::Value::try_from(origins)?);
    }
    Ok(result)
}

struct Adapter {
    process: OwnedProcess,
    events: Lines<BufReader<ChildStdout>>,
    code: Vec<u8>,
}
struct Frontend {
    project: PathBuf,
    source: PathBuf,
    session: PathBuf,
    library: String,
    backend: SocketAddr,
}
impl Frontend {
    fn state(&self, directory: &Path, installation: &Installation) -> Value {
        json!({"project":self.project, "source":self.source, "session":self.session, "library":self.library,
            "generation":directory, "backend":format!("http://{}", self.backend), "origin":installation.origin,
            "origins":installation.origins, "listenHost":installation.listen.ip().to_string(), "listenPort":installation.listen.port()})
    }
}
impl Adapter {
    async fn start(
        frontend: &Frontend,
        directory: &Path,
        installation: &Installation,
        code: &[u8],
        runner: &Runner,
    ) -> Result<Self> {
        let helper = frontend.session.join("web-dev.mjs");
        std::fs::write(&helper, code)?;
        let settings = frontend.session.join("vite.json");
        std::fs::write(
            &settings,
            serde_json::to_vec(&frontend.state(directory, installation))?,
        )?;
        let mut command = Command::new("node");
        command
            .arg(helper)
            .arg(settings)
            .current_dir(&frontend.project)
            .env_remove("SNAP_MASTER_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut process = runner.spawn(&mut command)?;
        let events = BufReader::new(
            process
                .child
                .stdout
                .take()
                .context("Missing adapter stdout")?,
        )
        .lines();
        let mut adapter = Self {
            process,
            events,
            code: code.to_vec(),
        };
        adapter.expect("ready", runner).await?;
        Ok(adapter)
    }
    async fn expect(&mut self, event: &str, runner: &Runner) -> Result<()> {
        tokio::select! {
            result = runner.cancelled() => result,
            result = tokio::time::timeout(Duration::from_secs(15), self.events.next_line()) => {
                let line = result.context("Vite adapter readiness timed out")??.context("Vite adapter exited before readiness")?;
                let message: Value = serde_json::from_str(&line).context("Invalid Vite adapter event")?;
                ensure!(message["event"] == event, "Unexpected Vite adapter event");
                Ok(())
            }
        }
    }
    async fn publish(&mut self, state: Value, runner: &Runner) -> Result<()> {
        let command = serde_json::to_vec(&json!({"command":"publish", "state": state}))?;
        let stdin = self
            .process
            .child
            .stdin
            .as_mut()
            .context("Missing adapter stdin")?;
        stdin.write_all(&command).await?;
        stdin.write_all(b"\n").await?;
        self.expect("published", runner).await
    }
}

async fn launch(
    directory: &Path,
    server_args: &[String],
    installation: &Installation,
    backend: SocketAddr,
    runner: &Runner,
) -> Result<OwnedProcess> {
    let mut command = Command::new(directory.join("server"));
    command
        .args(server_args)
        .arg("--config")
        .arg(directory.join("config.toml"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command.env_remove("SNAP_MASTER_KEY");
    if let Some(key) = &installation.key {
        command.env("SNAP_MASTER_KEY", key);
    }
    let mut process = runner.spawn(&mut command)?;
    process.forward_output();
    let authority = url::Url::parse(&installation.origin)?;
    let host = authority[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
    let ready = async {
        for _ in 0..150 {
            runner.check()?;
            ensure!(
                process.child.try_wait()?.is_none(),
                "Development backend exited before readiness; explicitly migrate its database first"
            );
            if http_ready(backend, &host).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("Development backend readiness timed out")
    };
    let result = tokio::select! { result = runner.cancelled() => result, result = ready => result };
    if let Err(error) = result {
        process.stop().await?;
        return Err(error);
    }
    Ok(process)
}

/// Startup reachability, not application health: any final HTTP status counts.
/// Direct loopback IO ignores proxy settings and never follows redirects. The
/// entire connect/write/read attempt has one 300ms timeout.
async fn http_ready(backend: SocketAddr, host: &str) -> std::io::Result<()> {
    tokio::time::timeout(Duration::from_millis(300), async {
        let mut stream = tokio::net::TcpStream::connect(backend).await?;
        stream
            .write_all(
                format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await?;
        // Read only a bounded status line; a stalled body is irrelevant to startup.
        let mut line = Vec::new();
        BufReader::new(stream.take(1024))
            .read_until(b'\n', &mut line)
            .await?;
        let valid = std::str::from_utf8(&line)
            .ok()
            .and_then(|line| line.strip_suffix("\r\n"))
            .and_then(|line| line.split_once(' '))
            .is_some_and(|(version, rest)| {
                let status = rest.split(' ').next().unwrap_or("");
                matches!(version, "HTTP/1.0" | "HTTP/1.1")
                    && status.len() == 3
                    && status.bytes().all(|byte| byte.is_ascii_digit())
                    && status
                        .parse::<u16>()
                        .is_ok_and(|status| (200..600).contains(&status))
            });
        if valid {
            Ok(())
        } else {
            Err(std::io::Error::other("Invalid readiness HTTP status line"))
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

#[derive(Default)]
struct Changes {
    build: bool,
    config: bool,
    adapter: bool,
}
impl Changes {
    fn add(&mut self, other: Self) {
        self.build |= other.build;
        self.config |= other.config;
        self.adapter |= other.adapter;
    }
    fn any(&self) -> bool {
        self.build || self.config || self.adapter
    }
}
fn changes(event: notify::Event, configuration: &Path, adapter: &Path) -> Changes {
    let mut result = Changes::default();
    if matches!(event.kind, notify::EventKind::Access(_)) {
        return result;
    }
    let workspace = adapter
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent);
    for path in event.paths {
        if path == configuration
            || path.parent() == configuration.parent()
                && matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("secrets.enc" | "secrets.key")
                )
        {
            result.config = true;
            continue;
        }
        if path == adapter {
            result.adapter = true;
            continue;
        }
        let relative = workspace
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(&path);
        if relative.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(
                    "target" | "dist" | "node_modules" | ".snap" | ".git" | ".tmp" | ".deployment"
                )
            )
        }) {
            continue;
        }
        if path.extension().is_some_and(|extension| extension == "rs")
            || path.extension().is_some_and(|extension| extension == "ts")
                && relative
                    .components()
                    .any(|component| component.as_os_str() == "server")
            || relative
                .components()
                .any(|component| component.as_os_str() == "prompts")
            || matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(
                    "Cargo.toml"
                        | "Cargo.lock"
                        | "package.json"
                        | "bun.lock"
                        | "snap.toml"
                        | "server.tsx"
                        | "auth-ui.tsx"
                )
            )
        {
            result.build = true;
        }
    }
    result
}

pub async fn run(project: Project, runner: &Runner, configuration: Option<PathBuf>) -> Result<()> {
    let settings = project.config.build();
    let server_args = &settings.server(&settings.development_server)?.args;
    let web = settings.web(&settings.development_client)?;
    let configuration = match configuration {
        Some(configuration) => configuration,
        None => snap_config::development_config(&project.root)?,
    };
    let configuration = configuration
        .canonicalize()
        .context("Cannot open config.toml")?;
    let mut installation =
        Installation::load(&configuration, runner, None, &project.config.application).await?;
    settings.validate()?;
    let (_, metadata) = cargo::metadata(&project, runner, &web.wasm).await?;
    let workspace = PathBuf::from(
        metadata["workspace_root"]
            .as_str()
            .context("Missing Cargo workspace")?,
    );
    let wasm = cargo::selected_package(&metadata, &project.file(&web.wasm)?)?;
    let library = wasm["targets"]
        .as_array()
        .context("Missing Wasm targets")?
        .iter()
        .find(|target| {
            target["crate_types"]
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"))
        })
        .and_then(|target| target["name"].as_str())
        .context("Missing Wasm library")?
        .to_owned();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })?;
    let mut watched = BTreeSet::new();
    for package in metadata["packages"]
        .as_array()
        .context("Missing Cargo packages")?
    {
        if let Some(manifest) = package["manifest_path"].as_str() {
            let directory = Path::new(manifest)
                .parent()
                .context("Missing package directory")?;
            if watched.insert(directory.to_owned()) {
                watcher.watch(directory, RecursiveMode::Recursive)?;
            }
        }
    }
    watcher.watch(&workspace, RecursiveMode::NonRecursive)?;
    watcher.watch(
        configuration.parent().context("Missing config directory")?,
        RecursiveMode::NonRecursive,
    )?;
    let adapter_source = workspace.join("tools/cli/web-dev.mjs");
    std::fs::create_dir_all(project.root.join(".snap"))?;
    let session = tempfile::Builder::new()
        .prefix("dev-")
        .tempdir_in(project.root.join(".snap"))?;
    let backend = TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let frontend = Frontend {
        project: project.root.clone(),
        source: project.root.join(&web.source),
        session: session.path().to_owned(),
        library,
        backend,
    };
    let mut running: Option<OwnedProcess> = None;
    let mut adapter: Option<Adapter> = None;
    let mut current: Option<(PathBuf, Installation)> = None;
    let mut serial = 0_u64;
    let title = {
        let mut chars = project.config.application.chars();
        format!("{}{}", chars.next().unwrap().to_uppercase(), chars.as_str())
    };
    let result = async {
        let mut pending = Changes { build: true, ..Changes::default() };
        loop {
            runner.check()?;
            if pending.any() {
                if pending.adapter && !pending.build && !pending.config
                    && let (Some(old), Some((directory, installed))) = (&mut adapter, &current) {
                        let previous_code = old.code.clone();
                        let code = adapter_code(&adapter_source)?;
                        old.process.stop().await?;
                        adapter = None;
                        match Adapter::start(&frontend, directory, installed, &code, runner).await {
                            Ok(next) => { adapter = Some(next); println!("{title} Vite adapter restarted"); }
                            Err(error) => {
                                runner.check()?;
                                adapter = Some(Adapter::start(&frontend, directory, installed, &previous_code, runner).await?);
                                eprintln!("Vite adapter rejected; previous adapter retained: {error:#}");
                            }
                        }
                        pending = Changes::default();
                        continue;
                }
                if pending.config {
                    match Installation::load(&configuration, runner, Some(installation.listen), &project.config.application).await {
                        Ok(next) => installation = next,
                        Err(error) => { runner.check()?; eprintln!("Configuration rejected; previous generation retained: {error:#}"); pending = Changes::default(); continue; }
                    }
                }
                serial += 1;
                let candidate = session.path().join(serial.to_string());
                let prepared = async {
                    // Do not pair a rebuilt executable with the previous command mode.
                    // This supervisor retains its startup arguments for launch and rollback.
                    let selected = Project::discover(Some(project.root.clone()))?;
                    let selected_build = selected.config.build();
                    ensure!(&selected_build.server(&selected_build.development_server)?.args == server_args,
                        "Server arguments changed; restart snap dev to apply them");
                    ensure!(selected_build.web(&selected_build.development_client)? == web, "Browser client selection changed; restart snap dev to apply it");
                    if let Some((directory, _)) = current.as_ref().filter(|_| !pending.build) {
                        // Configuration-only changes do not recompile application code.
                        build::copy_tree(directory, &candidate)?;
                    } else {
                        build::run(build::Args { environment: "development".into(), project: Some(project.root.clone()), web_only: false, client: None, server: None, target: Vec::new(), output: Some(candidate.clone()) }, runner).await?;
                    }
                    installation.write_generation(&candidate, backend)?;
                    runner.run(Command::new(candidate.join("server")).args(server_args).arg("--check-config").arg("--config").arg(candidate.join("config.toml")).env_remove("SNAP_MASTER_KEY"), false).await?;
                    Ok::<_, anyhow::Error>(())
                }.await;
                if let Err(error) = prepared {
                    runner.check()?;
                    if current.is_none() { return Err(error); }
                    installation = current.as_ref().unwrap().1.clone();
                    eprintln!("Rebuild failed; previous generation retained: {error:#}");
                    pending = Changes::default();
                    continue;
                }
                let mut newer = Changes::default();
                while let Ok(event) = receiver.try_recv() { newer.add(changes(event?, &configuration, &adapter_source)); }
                if newer.any() { pending.add(newer); continue; }
                if let Some(process) = &mut running { process.stop().await?; }
                running = None;
                match launch(&candidate, server_args, &installation, backend, runner).await {
                    Ok(process) => running = Some(process),
                    Err(error) => {
                        runner.check()?;
                        if let Some((directory, previous)) = &current {
                            running = Some(launch(directory, server_args, previous, backend, runner).await?);
                            installation = previous.clone();
                            eprintln!("Restart failed; previous generation retained: {error:#}");
                            pending = Changes::default();
                            continue;
                        }
                        return Err(error);
                    }
                }
                let restart_adapter = pending.adapter || current.as_ref().is_some_and(|(_, old)| old.listen != installation.listen || old.origin != installation.origin || old.origins != installation.origins);
                let previous_code = adapter.as_ref().map(|old| old.code.clone());
                let published = async {
                    if restart_adapter {
                        if let Some(old) = &mut adapter { old.process.stop().await?; }
                        adapter = None;
                    }
                    if let Some(adapter) = &mut adapter {
                        adapter.publish(frontend.state(&candidate, &installation), runner).await?;
                    } else {
                        adapter = Some(Adapter::start(&frontend, &candidate, &installation, &adapter_code(&adapter_source)?, runner).await?);
                    }
                    Ok::<_, anyhow::Error>(())
                }.await;
                if let Err(error) = published {
                    runner.check()?;
                    if let (Some((directory, previous)), Some(code)) = (&current, previous_code) {
                        // Publication includes Vite readiness. Restore both owners if
                        // its new listener/code fails, without overlapping DB opens.
                        if let Some(old) = &mut adapter { old.process.stop().await?; }
                        adapter = None;
                        if let Some(process) = &mut running { process.stop().await?; }
                        running = None;
                        running = Some(launch(directory, server_args, previous, backend, runner).await?);
                        adapter = Some(Adapter::start(&frontend, directory, previous, &code, runner).await?);
                        installation = previous.clone();
                        eprintln!("Frontend replacement failed; previous generation retained: {error:#}");
                        pending = Changes::default();
                        continue;
                    }
                    return Err(error);
                }
                if restart_adapter || current.is_none() {
                    println!("{title} dev origins: {}", installation.origins.join(", "));
                    println!("{title} dev {} (frontend HMR; Rust/Wasm rebuild and reload)", installation.origin);
                }
                println!("{title} generation ready: {serial}");
                current = Some((candidate, installation.clone()));
                pending = Changes::default();
            }
            tokio::select! {
                result = runner.cancelled() => return result,
                event = receiver.recv() => {
                    pending.add(changes(event.context("Development watcher stopped")??, &configuration, &adapter_source));
                    if pending.any() {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        while let Ok(event) = receiver.try_recv() { pending.add(changes(event?, &configuration, &adapter_source)); }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if let Some(process) = &mut running { ensure!(process.child.try_wait()?.is_none(), "Development backend exited unexpectedly"); }
                    if let Some(adapter) = &mut adapter { ensure!(adapter.process.child.try_wait()?.is_none(), "Vite adapter exited unexpectedly"); }
                }
            }
        }
    }.await;
    if let Some(adapter) = &mut adapter {
        let _ = adapter.process.stop().await;
    }
    if let Some(process) = &mut running {
        let _ = process.stop().await;
    }
    result
}
fn adapter_code(path: &Path) -> Result<Vec<u8>> {
    if path.is_file() {
        Ok(std::fs::read(path)?)
    } else {
        Ok(include_bytes!("../web-dev.mjs").to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn readiness_requires_a_bounded_http_status_not_just_a_connection() {
        let responses = [
            (Some("HTTP/1.1 200 OK\r\n".to_owned()), true),
            (Some("HTTP/1.0 404 Not Found\r\n".to_owned()), true),
            (
                Some("HTTP/1.1 500 Internal Server Error\r\n".to_owned()),
                true,
            ),
            (Some("HTTP/1.1 302 Found\r\n".to_owned()), true),
            (Some("HTTP/1.1 100 Continue\r\n".to_owned()), false),
            (Some("not HTTP\r\n".to_owned()), false),
            (Some("HTTP/1.1 20 OK\r\n".to_owned()), false),
            (Some("HTTP/1.1 200 OK".to_owned()), false),
            (
                Some(format!("HTTP/1.1 200 {}\r\n", "x".repeat(1024))),
                false,
            ),
            (Some(String::new()), false),
            (None, false),
        ];
        for (response, expected) in responses {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (finish, finished) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                    request.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                assert_eq!(
                    request,
                    "GET / HTTP/1.1\r\nHost: example.test:8080\r\nConnection: close\r\n\r\n"
                );
                if let Some(response) = response {
                    // Separate writes exercise partial status-line delivery.
                    let (first, rest) = response.split_at(response.len().min(5));
                    stream.get_mut().write_all(first.as_bytes()).await.unwrap();
                    tokio::task::yield_now().await;
                    stream.get_mut().write_all(rest.as_bytes()).await.unwrap();
                } else {
                    // A listening socket with no HTTP service is not ready.
                    let _ = finished.await;
                }
            });
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                http_ready(address, "example.test:8080"),
            )
            .await
            .expect("Readiness attempt did not enforce its timeout");
            let _ = finish.send(());
            peer.await.unwrap();
            assert_eq!(result.is_ok(), expected, "{result:?}");
        }
    }
}
