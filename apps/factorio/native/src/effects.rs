//! Git and OpenCode effects. No Store borrows cross IO. Integration's exact commit
//! is journaled before changing mainline, then reconciled by ancestry on restart.
use factorio::{Config, Session};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::process::Command;

struct Group(u32);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0 as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

pub async fn run(dir: &str, argv: &[&str], env: &[(&str, String)]) -> Result<String, String> {
    let (program, args) = argv.split_first().ok_or("Empty command")?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .kill_on_drop(true)
        .envs(env.iter().cloned())
        .process_group(0);
    // Hooks must finish and may not launch detached services. A finite command is
    // owned until its exit; resource paths survive failures for explicit recovery.
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let group = Group(child.id().ok_or("Command has no process ID")?);
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| "Command timed out")?
        .map_err(|e| e.to_string())?;
    drop(group);
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}",
            program,
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2000)
                .collect::<String>()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().into())
        .map_err(|_| "Command output is not UTF-8".into())
}
pub async fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["git", "-c", "core.hooksPath=/dev/null"];
    argv.extend_from_slice(args);
    run(dir, &argv, &[]).await
}
pub async fn head(c: &Config) -> Result<String, String> {
    git(
        &c.repository,
        &["rev-parse", &format!("refs/heads/{}", c.mainline)],
    )
    .await
}
pub async fn clean(path: &str) -> Result<(), String> {
    if !git(path, &["status", "--porcelain", "--untracked-files=all"])
        .await?
        .is_empty()
    {
        return Err(format!("Dirty worktree preserved at {path}"));
    }
    Ok(())
}
pub async fn verify_worktree(s: &Session) -> Result<(), String> {
    if git(&s.worktree, &["symbolic-ref", "--short", "HEAD"]).await? != s.branch {
        return Err("Worktree branch changed; inspect before recovery".into());
    }
    Ok(())
}
pub async fn setup(c: &Config, s: &Session) -> Result<(), String> {
    std::fs::create_dir_all(Path::new(&s.worktree).parent().ok_or("Invalid worktree")?)
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&s.data).map_err(|e| e.to_string())?;
    if !Path::new(&s.worktree).exists() {
        let branch = format!("refs/heads/{}", s.branch);
        match git(&c.repository, &["rev-parse", "--verify", &branch]).await {
            Ok(oid) if oid == s.base => {
                git(&c.repository, &["worktree", "add", &s.worktree, &s.branch]).await?;
            }
            Ok(_) => return Err("Existing session branch moved; refusing to reuse it".into()),
            Err(_) => {
                git(
                    &c.repository,
                    &["worktree", "add", "-b", &s.branch, &s.worktree, &s.base],
                )
                .await?;
            }
        }
    }
    verify_worktree(s).await?;
    let probe = std::net::TcpListener::bind(("127.0.0.1", s.port))
        .map_err(|_| "Allocated port is occupied")?;
    drop(probe);
    hook(c, s, &c.setup).await?;
    // Supplying a stable ID makes an interrupted create recoverable without
    // allocating a second conversation. Existing conversations are moved explicitly.
    let path = format!("/api/session/{}", s.conversation);
    if opencode("get", &path, None).await.is_err() {
        opencode("post", "/api/session", Some(json!({"id":s.conversation,"title":format!("Factorio {}",s.id),"location":{"directory":s.worktree}}))).await?;
    }
    opencode(
        "post",
        &format!("{path}/move"),
        Some(json!({"directory":s.worktree})),
    )
    .await?;
    Ok(())
}
pub async fn opencode(method: &str, path: &str, body: Option<Value>) -> Result<String, String> {
    // CLI owns V2 service discovery and authentication. Never read global config.
    let binary = std::env::var("FACTORIO_OPENCODE").unwrap_or_else(|_| "opencode".into());
    let mut args = vec![binary.as_str(), "api", method, path];
    let data = body.map(|v| v.to_string());
    if let Some(data) = data.as_deref() {
        args.extend(["--data", data]);
    }
    run(".", &args, &[]).await
}
pub async fn hook(c: &Config, s: &Session, argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Ok(());
    }
    let env = [
        ("FACTORIO_SESSION", s.id.clone()),
        ("PORT", s.port.to_string()),
        ("SNAP_DATABASE", format!("{}/store.sqlite", s.data)),
        ("FACTORIO_DATA", s.data.clone()),
        ("FACTORIO_REPOSITORY", c.repository.clone()),
    ];
    // A waiting child cannot execute a hook until its PID and Linux birth marker
    // are durable. Parent death before that handshake closes stdin and exits it.
    // Restart kills only the still-matching owned group, never a recycled PID.
    reap_hook(s)?;
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "read -r go || exit 1; \"$@\" </dev/null >/dev/null; code=$?; printf '%s\\n' \"$code\"; read -r finish", "factorio-hook"]).args(argv).current_dir(&s.worktree).envs(env).process_group(0).kill_on_drop(true).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id().ok_or("Missing hook PID")?;
    let _group = Group(pid);
    let marker = birth(pid).ok_or("Hook exited before its launch record")?;
    let path = Path::new(&s.data).join("process.json");
    let next = path.with_extension("pending");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&next).map_err(|e| e.to_string())?;
        file.write_all(json!({"pid":pid,"birth":marker}).to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&next, &path).map_err(|e| e.to_string())?;
        std::fs::File::open(&s.data)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut input = child.stdin.take().ok_or("Missing hook handshake")?;
    input.write_all(b"go\n").await.map_err(|e| e.to_string())?;
    let mut output = tokio::io::BufReader::new(child.stdout.take().ok_or("Missing hook result")?);
    let mut code = String::new();
    let result = tokio::time::timeout(Duration::from_secs(120), output.read_line(&mut code)).await;
    reap_hook(s)?;
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    result
        .map_err(|_| "Hook timed out")?
        .map_err(|e| e.to_string())?;
    if code.trim() != "0" {
        return Err(format!(
            "Hook failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2000)
                .collect::<String>()
        ));
    }
    Ok(())
}
fn birth(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
pub fn reap_hook(s: &Session) -> Result<(), String> {
    let path = Path::new(&s.data).join("process.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let pid = record["pid"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|p| *p > 1)
        .ok_or("Invalid process record")?;
    let marker = record["birth"].as_str().ok_or("Invalid process birth")?;
    if birth(pid).as_deref() == Some(marker) {
        nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        )
        .map_err(|e| e.to_string())?;
    }
    std::fs::remove_file(path).map_err(|e| e.to_string())?;
    Ok(())
}
pub async fn candidate(c: &Config, s: &Session) -> Result<(String, String), String> {
    verify_worktree(s).await?;
    clean(&s.worktree).await?;
    let commit = git(&s.worktree, &["rev-parse", "HEAD"]).await?;
    git(
        &s.worktree,
        &["merge-base", "--is-ancestor", &s.base, &commit],
    )
    .await
    .map_err(|_| "Candidate rewrote its recorded base")?;
    if commit == s.base {
        return Err("No candidate changes".into());
    }
    if !s.modules.iter().any(|m| m == "*") {
        let paths = git(
            &s.worktree,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                &s.base,
                &commit,
            ],
        )
        .await?;
        for path in paths.split('\0').filter(|p| !p.is_empty()) {
            if !s.modules.iter().any(|m| {
                c.modules
                    .get(m)
                    .is_some_and(|dir| path == dir || path.starts_with(&format!("{dir}/")))
            }) {
                return Err(format!(
                    "Unclaimed change: {path}. Expand scope before editing."
                ));
            }
        }
    }
    let target = head(c).await?;
    git(
        &c.repository,
        &[
            "update-ref",
            &format!("refs/factorio/candidates/{}/{}", s.id, commit),
            &commit,
        ],
    )
    .await?;
    Ok((commit, target))
}
pub async fn prepare(c: &Config, s: &Session) -> Result<String, String> {
    let candidate = s.candidate.as_ref().ok_or("No candidate")?;
    let (commit, target) = self::candidate(c, s).await?;
    if commit != candidate.commit || target != candidate.target {
        return Err(
            "Candidate or mainline moved. Publish again and obtain new human approval.".into(),
        );
    }
    main_checkout(c).await?;
    let tree = git(
        &c.repository,
        &["merge-tree", "--write-tree", &target, &commit],
    )
    .await?;
    let tree = tree.lines().next().ok_or("Missing merge tree")?;
    let planned = git(
        &c.repository,
        &[
            "-c",
            "user.name=Factorio",
            "-c",
            "user.email=factorio@localhost",
            "commit-tree",
            tree,
            "-p",
            &target,
            "-p",
            &commit,
            "-m",
            &format!("Factorio {}: accepted {}", s.id, commit),
        ],
    )
    .await?;
    git(
        &c.repository,
        &[
            "update-ref",
            &format!("refs/factorio/integration/{}", s.id),
            &planned,
        ],
    )
    .await?;
    Ok(planned)
}
async fn main_checkout(c: &Config) -> Result<(), String> {
    if git(&c.repository, &["symbolic-ref", "--short", "HEAD"]).await? != c.mainline {
        return Err("Main checkout must be on configured mainline".into());
    }
    clean(&c.repository).await
}
pub async fn integrate(c: &Config, s: &Session) -> Result<(), String> {
    let planned = s.integration.as_ref().ok_or("Missing integration intent")?;
    let target = head(c).await?;
    if git(
        &c.repository,
        &["merge-base", "--is-ancestor", planned, &target],
    )
    .await
    .is_ok()
    {
        return Ok(());
    }
    let candidate = s.candidate.as_ref().ok_or("Missing candidate")?;
    if target != candidate.target {
        return Err(
            "Mainline moved after integration intent; manual reconciliation required".into(),
        );
    }
    main_checkout(c).await?;
    git(&c.repository, &["merge", "--ff-only", "--no-edit", planned]).await?;
    Ok(())
}
pub async fn cleanup(c: &Config, s: &Session) -> Result<(), String> {
    if Path::new(&s.worktree).exists() {
        verify_worktree(s).await?;
        hook(c, s, &c.teardown).await?;
        if s.phase == factorio::Phase::Cleanup {
            clean(&s.worktree).await?;
            let work = git(&s.worktree, &["rev-parse", "HEAD"]).await?;
            git(
                &c.repository,
                &["merge-base", "--is-ancestor", &work, &head(c).await?],
            )
            .await
            .map_err(|_| "Unmerged work preserved")?;
            git(&c.repository, &["worktree", "remove", &s.worktree]).await?;
        }
        // Abandon explicitly releases claims, but preserves even clean unmerged work.
    }
    Ok(())
}
