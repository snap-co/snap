//! Every external check/test command gets an owned process group.
use anyhow::{Context, Result};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    fmt,
    io::Write,
    path::Path,
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

#[derive(Clone)]
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

    pub async fn cancelled(&self) -> Result<()> {
        let mut stopped = self.stopped.clone();
        self.check()?;
        stopped.changed().await?;
        self.check()
    }

    /// Long-lived development children have the same process-group lifetime as
    /// finite commands. Dropping their owner also retires all descendants.
    pub fn spawn(&self, command: &mut Command) -> Result<OwnedProcess> {
        self.check()?;
        command.process_group(0).kill_on_drop(true);
        let child = command.spawn().context("Could not start process")?;
        let group = Group(Pid::from_raw(
            child.id().context("Missing child PID")? as i32
        ));
        Ok(OwnedProcess {
            child,
            _group: group,
        })
    }

    pub async fn run(&self, command: &mut Command, capture: bool) -> Result<Vec<u8>> {
        let (status, output) = self.status(command, capture).await?;
        successful(status)?;
        Ok(output)
    }

    /// Capture both streams to separate evidence files without dumping successful
    /// child chatter into the parent report. Readers drain concurrently so a full
    /// stderr pipe cannot deadlock a command producing stdout. The same group
    /// ownership and cancellation policy applies to builds and parallel tests.
    /// Deadline expiry retires the entire group, and cannot become success even
    /// if a child handles termination by exiting with status zero.
    pub async fn logged(
        &self,
        command: &mut Command,
        log: &Path,
        verbose: bool,
        timeout: Option<Duration>,
    ) -> Result<Logged> {
        let mut stopped = self.stopped.clone();
        self.check()?;
        let stdout_file =
            std::io::BufWriter::new(std::fs::File::create(log.with_extension("out"))?);
        let stderr_file =
            std::io::BufWriter::new(std::fs::File::create(log.with_extension("err"))?);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = self.spawn(command)?;
        let stdout = drain(
            process.child.stdout.take().unwrap(),
            stdout_file,
            verbose,
            false,
        );
        let stderr = drain(
            process.child.stderr.take().unwrap(),
            stderr_file,
            verbose,
            true,
        );
        let status = async move {
            let deadline = async {
                if let Some(duration) = timeout {
                    tokio::time::sleep(duration).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let (status, timed_out) = tokio::select! {
                biased;
                _ = stopped.changed() => {
                    let signal = if *stopped.borrow() == 130 { Signal::SIGINT } else { Signal::SIGTERM };
                    (retire(&mut process.child, process._group.0, signal).await, false)
                },
                _ = deadline => (retire(&mut process.child, process._group.0, Signal::SIGTERM).await, true),
                status = process.child.wait() => (status, false),
            };
            drop(process);
            (status, timed_out)
        };
        // Retire descendants before waiting for EOF, including children that
        // inherited these pipes. This also bounds teardown of cancelled jobs.
        let ((status, timed_out), stdout, stderr) = tokio::try_join!(
            async {
                let (status, timed_out) = status.await;
                Ok::<_, anyhow::Error>((status?, timed_out))
            },
            stdout,
            stderr
        )?;
        Ok(Logged {
            status,
            stdout,
            stderr,
            timed_out,
        })
    }

    async fn status(&self, command: &mut Command, capture: bool) -> Result<(ExitStatus, Vec<u8>)> {
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
}

pub struct Logged {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl Logged {
    pub fn successful(&self) -> Result<()> {
        anyhow::ensure!(!self.timed_out, "Command exceeded its execution deadline");
        successful(self.status)
    }
}

async fn retire(child: &mut Child, group: Pid, signal: Signal) -> std::io::Result<ExitStatus> {
    let _ = killpg(group, signal);
    match tokio::time::timeout(Duration::from_secs(6), child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            let _ = killpg(group, Signal::SIGKILL);
            child.wait().await
        }
    }
}

async fn drain(
    mut input: impl tokio::io::AsyncRead + Unpin,
    mut file: std::io::BufWriter<std::fs::File>,
    verbose: bool,
    stderr: bool,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])?;
        output.extend_from_slice(&buffer[..count]);
        if verbose {
            if stderr {
                std::io::stderr().write_all(&buffer[..count])?;
            } else {
                std::io::stdout().write_all(&buffer[..count])?;
            }
        }
    }
    file.flush()?;
    Ok(output)
}

pub struct OwnedProcess {
    pub child: Child,
    _group: Group,
}
impl OwnedProcess {
    pub async fn stop(&mut self) -> Result<()> {
        let _ = killpg(self._group.0, Signal::SIGTERM);
        match tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await {
            Ok(status) => {
                status?;
            }
            Err(_) => {
                let _ = killpg(self._group.0, Signal::SIGKILL);
                self.child.wait().await?;
            }
        }
        Ok(())
    }

    pub fn forward_output(&mut self) {
        if let Some(stdout) = self.child.stdout.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{line}");
                }
            });
        }
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
