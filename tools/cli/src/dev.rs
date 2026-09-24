use crate::{
    build,
    config::Project,
    process::{Runner, Service},
    watch::{Changes, Sources},
};
use anyhow::{Context, Result, ensure};
use nix::{
    sys::signal::{Signal, kill},
    unistd::{Pid, Uid},
};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

pub async fn run(mut project: Project, runner: &Runner) -> Result<()> {
    eprintln!(
        "Developing {} ({})",
        project.config.application,
        project.root.display()
    );
    let preparation = build::prepare_dev(&project, runner).await?;
    let mut sources = Sources::new(&project, runner).await?;
    let artifacts = build::run(&project, runner, build::Mode::PreparedDev(preparation)).await?;
    let mut generation: Option<build::Generation> = None;
    let mut initial_changes = sources.settle(Changes::default()).await?;
    while initial_changes.native || initial_changes.web {
        project = Project::discover(Some(project.root.clone()))?;
        let mut next_sources = Sources::new(&project, runner).await?;
        let candidate = build::reload(
            &project,
            runner,
            initial_changes,
            generation.as_ref().map_or(&artifacts, |g| &g.artifacts),
        )
        .await?;
        let mut newer = sources.settle(Changes::default()).await?;
        newer.merge(next_sources.settle(Changes::default()).await?);
        sources = next_sources;
        if newer.native || newer.web {
            initial_changes.merge(newer);
            eprintln!("Discarding superseded initial Rust generation");
        } else {
            candidate.publish_bindings(&project)?;
            generation = Some(candidate);
            break;
        }
    }
    let mut build = build_token(&project)?;
    let mut running = launch(
        &project,
        generation.as_ref().map_or(&artifacts, |g| &g.artifacts),
        runner,
        &build,
        None,
    )
    .await?;
    let mut pending = None;
    let mut dirty = Changes::default();
    loop {
        let changes = match pending.take() {
            Some(changes) => changes,
            None => tokio::select! {
                result = running.wait(runner) => { running.stop().await?; return result; }
                changes = sources.next() => changes?,
            },
        };
        let mut changes = sources.settle(changes).await?;
        changes.merge(dirty);
        dirty = changes;
        runner.check()?;
        let candidate_project = match Project::discover(Some(project.root.clone())) {
            Ok(project) => project,
            Err(error) => {
                eprintln!("Configuration failed; previous generation retained: {error:#}");
                continue;
            }
        };
        let mut next_sources = match Sources::new(&candidate_project, runner).await {
            Ok(sources) => sources,
            Err(error) => {
                runner.check()?;
                eprintln!("Dependency discovery failed; previous generation retained: {error:#}");
                continue;
            }
        };
        changes.merge(sources.settle(Changes::default()).await?);
        dirty = changes;
        let previous = generation.as_ref().map_or(&artifacts, |g| &g.artifacts);
        eprintln!(
            "Rebuilding Rust: native={}, wasm={}",
            changes.native, changes.web
        );
        let candidate = tokio::select! {
            result = running.wait(runner) => { running.stop().await?; return result; }
            result = build::reload(&candidate_project, runner, changes, previous) => result,
        };
        let mut newer = sources.settle(Changes::default()).await?;
        newer.merge(next_sources.settle(Changes::default()).await?);
        sources = next_sources;
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                runner.check()?;
                eprintln!("Rebuild failed; previous generation retained: {error:#}");
                if newer.native || newer.web {
                    changes.merge(newer);
                    pending = Some(changes);
                }
                continue;
            }
        };
        if newer.native || newer.web {
            eprintln!("Discarding superseded Rust generation");
            changes.merge(newer);
            pending = Some(changes);
            continue;
        }
        let next_build = if changes.native || changes.config {
            build_token(&candidate_project)?
        } else {
            build.clone()
        };
        // Native replacement uses the same prescribed port. It necessarily has a
        // short outage. On startup failure restore the old executable and identity;
        // if restoration also fails, exit rather than claiming continued service.
        let addresses = (running.address, running.backend_address);
        if let Err(error) = running
            .switch(
                &candidate_project,
                &candidate.artifacts,
                runner,
                &next_build,
                changes,
            )
            .await
        {
            runner.check()?;
            eprintln!("Replacement failed; restoring previous generation: {error:#}");
            running.stop().await?;
            running = launch(&project, previous, runner, &build, Some(addresses))
                .await
                .context("Previous generation could not be restored")?;
            continue;
        }
        let newer = sources.settle(Changes::default()).await?;
        if newer.native || newer.web {
            if changes.native || changes.config {
                running.stop().await?;
                running = launch(&project, previous, runner, &build, Some(addresses)).await?;
            }
            changes.merge(newer);
            pending = Some(changes);
            eprintln!("Discarding superseded Rust generation");
            continue;
        }
        candidate.publish_bindings(&candidate_project)?;
        running.reload_browser(&candidate.artifacts, runner).await?;
        eprintln!("Rust generation ready: {next_build}");
        generation = Some(candidate);
        project = candidate_project;
        build = next_build;
        dirty = Changes::default();
    }
}

fn build_token(project: &Project) -> Result<String> {
    Ok(match std::env::var("SNAP_BUILD") {
        Ok(value) => value,
        Err(_) => format!(
            "{}-{}",
            project.config.application,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ),
    })
}

fn host_command(
    project: &Project,
    artifacts: &build::Artifacts,
    address: SocketAddr,
    build: &str,
) -> Command {
    let mut command = Command::new(&artifacts.executable);
    command
        .current_dir(&project.root)
        .env("SNAP_ADDR", address.to_string())
        .env("SNAP_BUILD", build)
        .env("SNAP_APPLICATION", &project.config.application)
        .env(
            "SNAP_ENV",
            std::env::var("SNAP_ENV").unwrap_or_else(|_| "development".into()),
        );
    // Never inherit another app's development assets.
    command.env_remove("SNAP_WEB_DIR");
    if let Some(web) = &artifacts.web {
        command.env("SNAP_WEB_DIR", web);
    }
    command
}

struct Running {
    backend: Service,
    frontend: Option<Service>,
    address: SocketAddr,
    backend_address: SocketAddr,
}

impl Running {
    async fn reload_browser(
        &mut self,
        artifacts: &build::Artifacts,
        runner: &Runner,
    ) -> Result<()> {
        if let (Some(frontend), Some(web)) = (&mut self.frontend, &artifacts.web) {
            let generation = artifacts
                .directory
                .file_name()
                .context("Missing generation name")?
                .to_string_lossy();
            frontend.send(&serde_json::json!({"wasm": web.join("snap_client_wasm_bg.wasm"), "generation": generation}).to_string()).await?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(1))
                .build()?;
            // Keep the previous files alive until Vite has switched its asset path.
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    runner.check()?;
                    if let Ok(reply) = client
                        .get(format!("{}/__snap/build", url(self.address)))
                        .send()
                        .await
                        && reply
                            .headers()
                            .get("x-snap-dev-generation")
                            .is_some_and(|header| header == generation.as_ref())
                    {
                        return Ok::<_, anyhow::Error>(());
                    }
                    runner.pause().await?;
                }
            })
            .await
            .context("Frontend generation acknowledgement timed out")??;
        }
        Ok(())
    }
    async fn wait(&mut self, runner: &Runner) -> Result<()> {
        if let Some(frontend) = &mut self.frontend {
            tokio::select! { result = self.backend.wait(runner) => result, result = frontend.wait(runner) => result }
        } else {
            self.backend.wait(runner).await
        }
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(frontend) = &mut self.frontend {
            let (back, front) = tokio::join!(self.backend.stop(), frontend.stop());
            back?;
            front?;
        } else {
            self.backend.stop().await?;
        }
        Ok(())
    }

    async fn switch(
        &mut self,
        project: &Project,
        artifacts: &build::Artifacts,
        runner: &Runner,
        build: &str,
        changes: Changes,
    ) -> Result<()> {
        if changes.config {
            self.stop().await?;
            let address = retain_port(project.address()?, self.address);
            let backend = retain_port(project.backend_address()?, self.backend_address);
            *self = launch(project, artifacts, runner, build, Some((address, backend))).await?;
        } else if changes.native {
            self.backend.stop().await?;
            self.backend = runner
                .service(
                    &mut host_command(project, artifacts, self.backend_address, build),
                    &url(self.backend_address),
                    build,
                )
                .await?;
        }
        Ok(())
    }
}

fn retain_port(mut requested: SocketAddr, previous: SocketAddr) -> SocketAddr {
    if requested.port() == 0 {
        requested.set_port(previous.port());
    }
    requested
}

async fn launch(
    project: &Project,
    artifacts: &build::Artifacts,
    runner: &Runner,
    build: &str,
    addresses: Option<(SocketAddr, SocketAddr)>,
) -> Result<Running> {
    let address = addresses.map_or_else(|| project.address(), |a| Ok(a.0))?;
    let mut command = host_command(project, artifacts, address, build);
    if let (Some(web), Some(assets)) = (&project.config.web, &artifacts.web) {
        let backend_address = addresses.map_or_else(|| project.backend_address(), |a| Ok(a.1))?;
        ensure!(
            address.port() == 0 || address.port() != backend_address.port(),
            "Frontend and backend must use different ports"
        );
        // Snap prescribes both addresses. Reserve port-zero requests here, never in
        // the child, and retain the public reservation until its launcher is ready.
        let (public, private) = if address.port() == 0 {
            let private = reserve(runner, backend_address).await?;
            (reserve(runner, address).await?, private)
        } else {
            let public = reserve(runner, address).await?;
            (public, reserve(runner, backend_address).await?)
        };
        let backend_address = private.local_addr()?;
        let address = public.local_addr()?;
        let backend_url = url(backend_address);
        let public_url = url(address);
        command
            .env("SNAP_ADDR", backend_address.to_string())
            .env("SNAP_WEB_DIR", assets);
        drop(private);
        let backend = runner.service(&mut command, &backend_url, build).await?;
        eprintln!("Backend ready at {backend_url}");
        let driver = project.root.join(".snap/dev-web.ts");
        std::fs::write(&driver, include_str!("../../../scripts/dev-web.ts"))?;
        drop(public);
        let frontend = runner
            .service(
                Command::new("bun")
                    .current_dir(&project.root)
                    .arg(&driver)
                    .arg(&project.root)
                    .arg(project.path(&web.package_dir))
                    .arg(project.file(&web.application)?)
                    .arg(project.file(&web.host)?)
                    .arg(project.file(&web.html)?)
                    .arg(assets.join("snap_client_wasm_bg.wasm"))
                    .arg(&backend_url)
                    .arg(address.to_string()),
                &public_url,
                build,
            )
            .await?;
        eprintln!("listening on {public_url}");
        Ok(Running {
            backend,
            frontend: Some(frontend),
            address,
            backend_address,
        })
    } else {
        let listener = reserve(runner, address).await?;
        let address = listener.local_addr()?;
        command.env("SNAP_ADDR", address.to_string());
        drop(listener);
        let backend = runner.service(&mut command, &url(address), build).await?;
        eprintln!("listening on {}", url(address));
        Ok(Running {
            backend,
            frontend: None,
            address,
            backend_address: address,
        })
    }
}

// Binding remains the final authority: another process can race the release and
// child bind. A failed child reports its error and exits; it never selects a port.
async fn reserve(runner: &Runner, address: SocketAddr) -> Result<TcpListener> {
    replace_listener(runner, address.port()).await?;
    TcpListener::bind(address)
        .with_context(|| format!("Cannot reserve development address {address}"))
}

fn url(mut address: SocketAddr) -> String {
    if address.ip().is_unspecified() {
        address.set_ip(match address.ip() {
            IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    format!("http://{address}")
}

async fn listeners(runner: &Runner, port: u16) -> Result<BTreeSet<i32>> {
    let (status, output) = runner
        .status(
            Command::new("lsof")
                .args(["-nP", "-t", "-a", "-u"])
                .arg(Uid::current().to_string())
                .arg(format!("-iTCP:{port}"))
                .arg("-sTCP:LISTEN"),
            true,
        )
        .await
        .context("snap dev requires lsof for development listener replacement")?;
    ensure!(
        status.success() || status.code() == Some(1),
        "lsof failed: {status}"
    );
    String::from_utf8(output)?
        .lines()
        .map(|line| line.parse().context("Invalid listener PID from lsof"))
        .collect()
}

async fn replace_listener(runner: &Runner, port: u16) -> Result<()> {
    if port == 0 {
        return Ok(());
    }
    let previous = listeners(runner, port).await?;
    if previous.is_empty() {
        return Ok(());
    }
    eprintln!("Replacing listener on port {port}: {previous:?}");
    for signal in [Signal::SIGTERM, Signal::SIGKILL] {
        let current = listeners(runner, port).await?;
        for pid in previous.intersection(&current) {
            let _ = kill(Pid::from_raw(*pid), signal);
        }
        for _ in 0..30 {
            if previous.is_disjoint(&listeners(runner, port).await?) {
                return Ok(());
            }
            runner.pause().await?;
        }
    }
    anyhow::bail!("Listener did not release port {port}")
}
