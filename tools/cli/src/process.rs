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
    io::AsyncReadExt,
    process::Command,
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
        if !status.success() {
            use std::os::unix::process::ExitStatusExt;
            return Err(Failed(
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8,
            )
            .into());
        }
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

struct Group(Pid);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = killpg(self.0, Signal::SIGKILL);
    }
}
