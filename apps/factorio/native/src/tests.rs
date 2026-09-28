use crate::effects;
use factorio::{Candidate, Config, Phase, Session};
use std::path::Path;

#[tokio::test]
#[ignore = "hook crash helper"]
async fn hook_child() {
    let Ok(path) = std::env::var("FACTORIO_HOOK_CHILD") else {
        return;
    };
    let (config, session): (Config, Session) =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    effects::hook(
        &config,
        &session,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "echo $$ > \"$FACTORIO_DATA/running\"; exec sleep 120".into(),
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "real process crash recovery"]
async fn restart_retires_owned_hook_group_after_host_kill() {
    let (_temp, c, s) = fixture().await;
    std::fs::create_dir_all(&s.data).unwrap();
    let input = Path::new(&s.data).join("fixture.json");
    std::fs::write(&input, serde_json::to_vec(&(&c, &s)).unwrap()).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::hook_child", "--ignored"])
        .env("FACTORIO_HOOK_CHILD", input)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let running = Path::new(&s.data).join("running");
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !running.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    child.kill().unwrap();
    child.wait().unwrap();
    effects::reap_hook(&s).unwrap();
    ready.unwrap();
    let pid = std::fs::read_to_string(running).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
            if stat.rsplit_once(") ").unwrap().1.starts_with('Z') {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!Path::new(&s.data).join("process.json").exists());
}
async fn write_commit(dir: &str, path: &str, value: &str) -> String {
    std::fs::write(Path::new(dir).join(path), value).unwrap();
    effects::git(dir, &["add", "--", path]).await.unwrap();
    effects::git(
        dir,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "fixture",
        ],
    )
    .await
    .unwrap();
    effects::git(dir, &["rev-parse", "HEAD"]).await.unwrap()
}
async fn fixture() -> (tempfile::TempDir, Config, Session) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let repo = repo.to_str().unwrap();
    effects::git(repo, &["init", "-b", "main"]).await.unwrap();
    std::fs::create_dir_all(Path::new(repo).join("crates/a")).unwrap();
    let base = write_commit(repo, "crates/a/file", "base").await;
    let c = Config {
        repository: repo.into(),
        mainline: "main".into(),
        modules: [("a".into(), "crates/a".into())].into_iter().collect(),
        resources: temp.path().join("resources").to_str().unwrap().into(),
        first_port: 12000,
        setup: vec![],
        teardown: vec![],
    };
    let worktree = temp.path().join("work").to_str().unwrap().to_owned();
    effects::git(repo, &["worktree", "add", "-b", "factorio/work", &worktree])
        .await
        .unwrap();
    let s = Session {
        id: "work".into(),
        owner: "fixture".into(),
        prompt: "fixture".into(),
        tickets: vec![],
        modules: vec!["a".into()],
        phase: Phase::Active,
        base,
        branch: "factorio/work".into(),
        worktree,
        data: temp.path().join("data").to_str().unwrap().into(),
        port: 12000,
        conversation: "ses_fixture".into(),
        candidate: None,
        publications: vec![],
        integration: None,
        error: String::new(),
        desired: factorio::Desired::Active,
    };
    (temp, c, s)
}
#[tokio::test]
#[ignore = "real Git acceptance"]
async fn merge_reconciliation_does_not_merge_twice_and_preserves_dirty_work() {
    let (_temp, c, mut s) = fixture().await;
    let oid = write_commit(&s.worktree, "crates/a/file", "implemented").await;
    let (commit, target) = effects::candidate(&c, &s).await.unwrap();
    assert_eq!(commit, oid);
    s.candidate = Some(Candidate {
        commit,
        target,
        evidence: "fixture".into(),
        findings: vec![],
        approval: None,
    });
    let plan = effects::prepare(&c, &s).await.unwrap();
    s.integration = Some(plan.clone());
    s.phase = Phase::Integrating;
    effects::integrate(&c, &s).await.unwrap();
    // Simulate loss of the Store publication after Git succeeded.
    effects::integrate(&c, &s).await.unwrap();
    assert_eq!(effects::head(&c).await.unwrap(), plan);
    assert_eq!(
        std::fs::read_to_string(Path::new(&c.repository).join("crates/a/file")).unwrap(),
        "implemented"
    );
    s.phase = Phase::Cleanup;
    std::fs::write(Path::new(&s.worktree).join("dirty"), "keep me").unwrap();
    assert!(effects::cleanup(&c, &s).await.is_err());
    assert!(Path::new(&s.worktree).join("dirty").exists());
    std::fs::remove_file(Path::new(&s.worktree).join("dirty")).unwrap();
    effects::cleanup(&c, &s).await.unwrap();
    assert!(!Path::new(&s.worktree).exists());
}
#[tokio::test]
#[ignore = "real Git acceptance"]
async fn moved_candidates_targets_and_unclaimed_changes_stop_integration() {
    let (_temp, c, mut s) = fixture().await;
    write_commit(&s.worktree, "crates/a/file", "first").await;
    let (commit, target) = effects::candidate(&c, &s).await.unwrap();
    s.candidate = Some(Candidate {
        commit,
        target,
        evidence: "fixture".into(),
        findings: vec![],
        approval: None,
    });
    write_commit(&s.worktree, "crates/a/file", "second").await;
    assert!(effects::prepare(&c, &s).await.is_err());
    let (commit, target) = effects::candidate(&c, &s).await.unwrap();
    s.candidate.as_mut().unwrap().commit = commit;
    s.candidate.as_mut().unwrap().target = target;
    write_commit(&c.repository, "mainline-file", "new target").await;
    assert!(effects::prepare(&c, &s).await.is_err());
    write_commit(&s.worktree, "unclaimed", "outside scope").await;
    assert!(
        effects::candidate(&c, &s)
            .await
            .unwrap_err()
            .contains("Unclaimed")
    );
    s.phase = Phase::Abandoning;
    effects::cleanup(&c, &s).await.unwrap();
    assert!(Path::new(&s.worktree).join("unclaimed").exists());
}
