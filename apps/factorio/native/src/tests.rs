use crate::effects;
use factorio::{Candidate, Config, Phase, Session};
use std::path::Path;
#[path = "../../../../platforms/transport/tests/support/mod.rs"]
mod tls_support;

#[test]
fn accepted_identity_uses_captured_authority_while_new_admissions_reject() {
    use snap_document_local::Host;
    use snap_transport::{Command, Event, Invocation, Response, json};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let mut store = snap_sqlite::Sqlite::memory(&crate::migrations()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(snap_oidc::relying_party::TABLES.iter())
        .chain(["factorio.agents", "factorio.cli", "factorio.cli_login"].iter())
    {
        store.load(table).unwrap();
    }
    let live = Arc::new(AtomicBool::new(true));
    let authority = live.clone();
    let host = Host::new(
        store,
        factorio::documents::document(),
        Arc::new(move |_, _| {
            if authority.load(Ordering::SeqCst) {
                Ok("captured-owner".into())
            } else {
                Err(snap_store::Error::NotFound)
            }
        }),
        Default::default(),
        "accepted-authority".into(),
    );
    let config = factorio::Config {
        repository: "/repo".into(),
        mainline: "main".into(),
        modules: Default::default(),
        resources: "/resources".into(),
        first_port: 15000,
        setup: vec![],
        teardown: vec![],
    };
    let mut host = crate::operations::register(host, config, "http://localhost".into());
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: "opaque-private-credential".into(),
            client_id: "identity-test".into(),
        },
        0,
    )
    .unwrap();
    assert_eq!(
        host.drain(peer).unwrap(),
        vec![Response::Attached { resumed: false }]
    );
    let invoke = |id| {
        Command::Invoke(Invocation {
            id,
            operation: "factorio.identity".into(),
            input: json!({}),
        })
    };
    host.submit(peer, invoke(1), 0).unwrap();
    assert_eq!(
        host.drain(peer).unwrap(),
        vec![Response::Events(vec![Event::Accepted { id: 1 }])]
    );
    live.store(false, Ordering::SeqCst);
    assert!(host.step());
    let completion = host.drain(peer).unwrap().into_iter().find_map(|response| {
        let Response::Events(events) = response else {
            return None;
        };
        events.into_iter().find_map(|event| match event {
            Event::Completed { id: 1, outcome } => Some(outcome),
            _ => None,
        })
    });
    assert_eq!(completion, Some(Ok(json!({"owner":"captured-owner"}))));
    assert!(host.submit(peer, invoke(2), 1).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real native CLI processes"]
async fn native_cli_login_intake_tools_and_authority_without_shell_environment() {
    use serde_json::json;
    use snap_document_local::{Host, web::Shared};
    use snap_oidc::relying_party as rp;
    let (temp, config, _) = fixture().await;
    let mut store = snap_sqlite::Sqlite::memory(&crate::migrations()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(["factorio.agents", "factorio.cli", "factorio.cli_login"].iter())
    {
        store.load(table).unwrap();
    }
    let owner = rp::owner("http://issuer", "subject");
    let session = rp::Session {
        id: rp::digest("browser"),
        owner: owner.clone(),
        subject: "subject".into(),
        issuer: "http://issuer".into(),
        csrf: "csrf".into(),
        nonce: "nonce".into(),
        profile: json!({}),
        tokens: rp::Tokens {
            access: "access".into(),
            refresh: "refresh".into(),
            id_token: "id".into(),
            access_expires: crate::now() + 3600,
            auth_time: None,
        },
        expires: crate::now() + 3600,
        refreshing: false,
        version: 1,
    };
    store
        .run("seed", |tx| {
            tx.insert(
                "oidc_rp.sessions",
                [
                    ("id".into(), session.id.clone().into()),
                    (
                        "data".into(),
                        serde_json::to_string(&session).unwrap().into(),
                    ),
                ]
                .into_iter()
                .collect(),
            )?;
            tx.insert(
                "factorio.agents",
                [
                    ("id".into(), rp::digest("agent").into()),
                    ("session".into(), session.id.clone().into()),
                ]
                .into_iter()
                .collect(),
            )?;
            Ok(())
        })
        .unwrap();
    let host = Host::new(
        store,
        factorio::documents::document(),
        std::sync::Arc::new(|tx, b| crate::operations::session(tx, b).map(|(s, _)| s.owner)),
        Default::default(),
        "cli-test".into(),
    );
    let host = crate::operations::register(host, config, "http://localhost".into());
    let shared = Shared::new(host, "http://localhost".into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (server_tls, client_tls) = tls_support::pki(temp.path(), false);
    let tcp = tokio::spawn(snap_document_local::tcp::serve(
        listener,
        shared.clone(),
        server_tls.clone(),
    ));
    let dispatch = tokio::spawn(snap_document_local::web::dispatch(shared.clone()));
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("factory");
    assert!(binary.is_file(), "Build factory-cli before this gate");
    let credentials = temp.path().join("credentials.json");
    async fn call(
        binary: &Path,
        credentials: &Path,
        args: &[&str],
        input: Option<serde_json::Value>,
    ) -> (bool, serde_json::Value, String) {
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(binary)
            .env_clear()
            .arg("--credentials")
            .arg(credentials)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .await
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        (
            result.status.success(),
            serde_json::from_slice(&result.stdout).unwrap_or(serde_json::Value::Null),
            String::from_utf8_lossy(&result.stderr).into(),
        )
    }
    let (ok, login, error) = call(
        &binary,
        &credentials,
        &[
            "login",
            "--addr",
            &addr,
            "--ca-file",
            temp.path().join("ca.pem").to_str().unwrap(),
            "--token",
            "agent",
        ],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(login["owner"], owner);
    assert!(login.get("bearer").is_none());
    assert!(login["expires"].as_i64().unwrap() <= crate::now() + 1800);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&credentials)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    let (ok, _, error) = call(&binary, &credentials, &["onboard"], None).await;
    assert!(ok, "{error}");
    let (ok, item, error) = call(
        &binary,
        &credentials,
        &["intake", "--no-open", "--", "a local task"],
        None,
    )
    .await;
    assert!(ok, "{error}");
    let id = item["id"].as_str().unwrap();
    let (ok, read, error) = call(
        &binary,
        &credentials,
        &["intake-read", "--intake", id],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(read["intake"]["revision"], 0);
    let drafts = json!({"revision":0,"route":"grill","rationale":"needs scope","tickets":[]});
    let mut watcher = tokio::process::Command::new(&binary)
        .env_clear()
        .arg("--credentials")
        .arg(&credentials)
        .arg("watch")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    use tokio::io::AsyncBufReadExt;
    let mut observations = tokio::io::BufReader::new(watcher.stdout.take().unwrap()).lines();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), observations.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(initial.contains("a local task"));
    let (ok, saved, error) = call(
        &binary,
        &credentials,
        &["intake-save", "-", "--intake", id],
        Some(drafts.clone()),
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(saved["revision"], 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let update = observations.next_line().await.unwrap().unwrap();
            if update.contains("needs scope") {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (ok, _, _) = call(
        &binary,
        &credentials,
        &["intake-save", "-", "--intake", id],
        Some(drafts),
    )
    .await;
    assert!(!ok, "Stale revision must reject");
    // Lose only the completion via a real TCP proxy, after delivering ACK. The
    // Host still owns the accepted draft write; a new process must recover its
    // exact invocation rather than allocating a fresh ID and applying it twice.
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap().to_string();
    let upstream_addr = addr.clone();
    let drop_handshake = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let proxy_drop_handshake = drop_handshake.clone();
    let proxy_server_tls = server_tls.clone();
    let proxy_client_tls = client_tls.clone();
    let proxy = tokio::spawn(async move {
        let mut first = true;
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = proxy_listener.accept() => {
                    let (down,_) = accepted.unwrap();
                    let proxy_server_tls = proxy_server_tls.clone();
                    let proxy_client_tls = proxy_client_tls.clone();
                    let upstream_addr = upstream_addr.clone();
                    let lose = first; first = false;
                    let lose_handshake=proxy_drop_handshake.swap(false,std::sync::atomic::Ordering::SeqCst);
                    peers.spawn(async move {
                        let mut down = proxy_server_tls.accept(down).await.unwrap();
                        let mut up = proxy_client_tls.connect(&upstream_addr).await.unwrap();
                        if !lose && !lose_handshake {let _=tokio::io::copy_bidirectional(&mut down,&mut up).await;return;}
                        let (mut down_read,mut down_write)=tokio::io::split(down);
                        let (mut up_read,mut up_write)=tokio::io::split(up);
                        let forwarding=tokio::spawn(async move {let _=tokio::io::copy(&mut down_read,&mut up_write).await;});
                        loop {
                            let (response,retention)=snap_transport_native::read_response(&mut up_read).await.unwrap();
                            if lose_handshake && matches!(response,snap_transport::Response::Attached {..}) {
                                 forwarding.abort();let _=forwarding.await;let _=tokio::io::AsyncWriteExt::shutdown(&mut down_write).await;break;
                            }
                            let accepted=matches!(&response,snap_transport::Response::Events(events) if events.iter().any(|e|matches!(e,snap_transport::Event::Accepted {..})));
                            snap_transport_native::write_response(&mut down_write,&response,matches!(response,snap_transport::Response::Attached {..}),retention.as_ref()).await.unwrap();
                             if accepted {forwarding.abort();let _=forwarding.await;let _=tokio::io::AsyncWriteExt::shutdown(&mut down_write).await;break;}
                        }
                    });
                },
                Some(_) = peers.join_next(), if !peers.is_empty() => {},
            }
        }
    });
    let (ok,_,_) = call(&binary,&credentials,&["--addr",&proxy_addr,"intake-save","-","--intake",id],Some(json!({"revision":1,"route":"triage","rationale":"accepted but response lost","tickets":[]}))).await;
    assert!(!ok, "The interrupted client must report an unknown outcome");
    let (ok, recovered, error) = call(&binary, &credentials, &["retry"], None).await;
    assert!(ok, "{error}");
    assert_eq!(recovered["revision"], 2);
    let (ok, read, error) = call(
        &binary,
        &credentials,
        &["intake-read", "--intake", id],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(read["intake"]["revision"], 2);
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert!(config["next_id"].as_u64().unwrap() > 5);
    let client_id = config["client_id"].as_str().unwrap();
    let mut reattach = snap_transport_native::Client::open(&addr, &client_tls)
        .await
        .unwrap();
    reattach
        .send(&snap_transport::Command::Connect {
            bearer: config["bearer"].as_str().unwrap().into(),
            client_id: client_id.into(),
        })
        .await
        .unwrap();
    assert_eq!(
        reattach.receive().await.unwrap().0,
        snap_transport::Response::Attached { resumed: true }
    );
    reattach
        .send(&snap_transport::Command::Close)
        .await
        .unwrap();
    let _ = reattach.receive().await;
    drop(reattach);
    let mut fenced = config;
    fenced["pending"] = json!({"id":fenced["next_id"],"operation":"factorio.intake-drafts","input":{"workspace":fenced["workspace"],"id":id,"drafts":{"revision":2,"route":"triage","rationale":"must never replay on new lifetime","tickets":[]}}});
    fenced["next_id"] = json!(fenced["next_id"].as_u64().unwrap() + 1);
    std::fs::write(&credentials, serde_json::to_vec(&fenced).unwrap()).unwrap();
    drop_handshake.store(true, std::sync::atomic::Ordering::SeqCst);
    let (ok, _, error) = call(&binary, &credentials, &["retry"], None).await;
    assert!(!ok && error.contains("TCP detached"), "{error}");
    let after_loss: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert_eq!(after_loss["pending_replayable"], true);
    assert_eq!(after_loss["lifetime"], fenced["lifetime"]);
    for _ in 0..2 {
        let (ok, _, error) = call(&binary, &credentials, &["retry"], None).await;
        assert!(!ok && error.contains("Logical lifetime ended"), "{error}");
    }
    // Explicit login is the only way to abandon the unknown result here.
    let (ok, _, error) = call(&binary, &credentials, &["login", "--token", "agent"], None).await;
    assert!(ok, "{error}");
    let (ok, read, error) = call(
        &binary,
        &credentials,
        &["intake-read", "--intake", id],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(read["intake"]["revision"], 2);
    for n in 1..=3 {
        let (ok,state,error)=call(&binary,&credentials,&["ticket","-"],Some(json!({"id":format!("large-ticket-{n}"),"title":format!("Large ticket {n}"),"description":"x".repeat(24000),"modules":["a"],"status":"draft","notes":"","parent":null,"blockers":[]}))).await;
        assert!(ok, "{error}");
        assert_eq!(
            state["tickets"][format!("large-ticket-{n}")]["description"]
                .as_str()
                .unwrap()
                .len(),
            24000
        );
    }
    let (ok, state, error) = call(&binary, &credentials, &["status"], None).await;
    assert!(ok, "{error}");
    assert_eq!(state["tickets"].as_object().unwrap().len(), 3);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let update = observations.next_line().await.unwrap().unwrap();
            if update.contains("Large ticket 3") {
                assert!(update.len() > 65536);
                assert!(update.contains("Large ticket 1") && update.contains("Large ticket 2"));
                break;
            }
        }
    })
    .await
    .unwrap();
    watcher.kill().await.unwrap();
    let _ = watcher.wait().await;
    let (ok, _, error) = call(&binary, &credentials, &["logout"], None).await;
    assert!(ok, "{error}");
    let (ok, _, _) = call(&binary, &credentials, &["status"], None).await;
    assert!(!ok);
    tcp.abort();
    dispatch.abort();
    proxy.abort();
    let _ = proxy.await;
    let _ = tcp.await;
    let _ = dispatch.await;
}

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
        created_at: None,
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
