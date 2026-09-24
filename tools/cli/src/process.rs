//! Every external command gets an owned process group, including build tools and hooks.
use anyhow::{Context, Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    fmt,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
    signal::unix::{SignalKind, signal},
    sync::watch,
};

#[derive(Debug)]
pub struct Failed(pub u8);
impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "process exited with status {}", self.0)
    }
}
impl std::error::Error for Failed {}

pub struct Runner {
    stopped: watch::Receiver<u8>,
}

impl Runner {
    /// Readiness is HTTP Build discovery, independent of the service's logging.
    /// One deadline bounds all probes; child exit and interruption remain observable.
    /// Dropping a partially started service releases its entire process group.
    pub async fn service(&self, command: &mut Command, url: &str, build: &str) -> Result<Service> {
        self.check()?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Could not launch {:?}", command.as_std().get_program()))?;
        let group = Group(Pid::from_raw(
            child.id().context("Missing service PID")? as i32
        ));
        let stderr = child.stderr.take().context("Missing service stderr")?;
        let logs = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Some(line) = lines.next_line().await? {
                eprintln!("[service] {line}");
            }
            Ok::<_, std::io::Error>(())
        });
        let mut service = Service {
            child,
            group: Some(group),
            logs: Some(logs),
        };
        let mut stopped = self.stopped.clone();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()?;
        let readiness = async {
            loop {
                if let Ok(reply) = client.get(format!("{url}/__snap/build")).send().await
                    && reply.status().is_success()
                    && let Ok(body) = reply.bytes().await
                    && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
                    && value["build"].as_str() == Some(build)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::select! {
            value = tokio::time::timeout(Duration::from_secs(20), readiness) => {
                value.context("Service readiness timed out")?;
            }
            status = service.child.wait() => {
                let status = status?;
                service.finish().await?;
                successful(status)?;
                anyhow::bail!("Service exited before readiness");
            }
            _ = stopped.changed() => { service.stop().await?; return Err(Failed(*stopped.borrow()).into()); }
        };
        Ok(service)
    }
    pub fn new() -> Result<Self> {
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let (sender, stopped) = watch::channel(0);
        tokio::spawn(async move {
            let code = tokio::select! { _ = interrupt.recv() => 130, _ = terminate.recv() => 143 };
            sender.send_replace(code);
            // Keep the channel alive until the receiver goes away.
            sender.closed().await;
        });
        Ok(Self { stopped })
    }

    pub fn check(&self) -> Result<()> {
        let code = *self.stopped.borrow();
        if code != 0 {
            return Err(Failed(code).into());
        }
        Ok(())
    }

    pub async fn run(&self, command: &mut Command, capture: bool) -> Result<Vec<u8>> {
        let (status, output) = self.status(command, capture).await?;
        successful(status)?;
        Ok(output)
    }

    pub async fn status(
        &self,
        command: &mut Command,
        capture: bool,
    ) -> Result<(ExitStatus, Vec<u8>)> {
        let mut stopped = self.stopped.clone();
        self.check()?;
        command
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("Could not launch {:?}", command.as_std().get_program()))?;
        let pid = Pid::from_raw(child.id().context("Missing child PID")? as i32);
        let group = Group(pid);
        let stdout = child.stdout.take();
        let reader = tokio::spawn(async move {
            let mut output = Vec::new();
            if let Some(mut stdout) = stdout {
                stdout.read_to_end(&mut output).await?;
            }
            Ok::<_, std::io::Error>(output)
        });
        let status = tokio::select! {
            biased;
            _ = stopped.changed() => {
                let signal = if *stopped.borrow() == 130 { Signal::SIGINT } else { Signal::SIGTERM };
                let _ = killpg(pid, signal);
                match tokio::time::timeout(Duration::from_secs(6), child.wait()).await {
                    Ok(status) => status?,
                    Err(_) => { let _ = killpg(pid, Signal::SIGKILL); child.wait().await? }
                }
            },
            status = child.wait() => status?,
        };
        // Descendants cannot outlive a completed command, or keep captured pipes open.
        drop(group);
        let output = reader.await??;
        Ok((status, output))
    }

    pub async fn pause(&self) -> Result<()> {
        let mut stopped = self.stopped.clone();
        self.check()?;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => Ok(()),
            _ = stopped.changed() => bail!(Failed(*stopped.borrow())),
        }
    }
}

pub struct Service {
    child: Child,
    group: Option<Group>,
    logs: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Service {
    pub async fn wait(&mut self, runner: &Runner) -> Result<()> {
        let mut stopped = runner.stopped.clone();
        let status = if *stopped.borrow() != 0 {
            self.terminate().await?
        } else {
            tokio::select! {
                status = self.child.wait() => status?,
                _ = stopped.changed() => self.terminate().await?,
            }
        };
        self.finish().await?;
        successful(status)
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.terminate().await?;
        self.finish().await
    }

    async fn terminate(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        if let Some(group) = &self.group {
            let _ = killpg(group.0, Signal::SIGTERM);
        }
        match tokio::time::timeout(Duration::from_secs(6), self.child.wait()).await {
            Ok(status) => Ok(status?),
            Err(_) => {
                if let Some(group) = &self.group {
                    let _ = killpg(group.0, Signal::SIGKILL);
                }
                Ok(self.child.wait().await?)
            }
        }
    }

    async fn finish(&mut self) -> Result<()> {
        self.group.take();
        if let Some(logs) = self.logs.take() {
            logs.await??;
        }
        Ok(())
    }
}

struct Group(Pid);
fn successful(status: ExitStatus) -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    if !status.success() {
        return Err(Failed(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8,
        )
        .into());
    }
    Ok(())
}
impl Drop for Group {
    fn drop(&mut self) {
        let _ = killpg(self.0, Signal::SIGKILL);
    }
}
