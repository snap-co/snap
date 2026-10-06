//! Paired production hosts plus the real external CLI, binary carrier and crypto.
//! Not selected by framework tests; this is an app-owned SSO integration gate.
use serde_json::{Value, json};
use snap_transport::{
    Command, Event, Invocation, Response,
    bearer::Change,
    native::{TcpClient, tls::ClientTls},
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[path = "../../../../crates/transport/tests/support/mod.rs"]
mod pki;
const THREAD: &str = "018f3c4b-6d2a-7000-8000-000000000001";
struct Host {
    child: Child,
    log: PathBuf,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Host {
    async fn start(binary: &Path, config: &Path, base: &str, log: PathBuf) -> Self {
        let migrate = ProcessCommand::new(binary)
            .args(["--config", config.to_str().unwrap(), "--migrate"])
            .env_remove("SNAP_MASTER_KEY")
            .output()
            .unwrap();
        assert!(
            migrate.status.success(),
            "{}",
            String::from_utf8_lossy(&migrate.stderr)
        );
        let file = std::fs::File::create(&log).unwrap();
        let child = ProcessCommand::new(binary)
            .args(["--config", config.to_str().unwrap()])
            .env_remove("SNAP_MASTER_KEY")
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .unwrap();
        let mut host = Self { child, log };
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if http
                .get(format!("{base}/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return host;
            }
            assert!(
                host.child.try_wait().unwrap().is_none(),
                "Host exited: {}",
                std::fs::read_to_string(&host.log).unwrap()
            );
            assert!(Instant::now() < deadline, "Host startup timed out");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
struct Requests {
    addr: String,
    tls: ClientTls,
}
async fn request(
    requests: &mut Requests,
    name: &str,
    input: Value,
    bearer: Option<String>,
) -> (snap_transport::Outcome, Option<String>) {
    let mut tcp = TcpClient::open(&requests.addr, &requests.tls)
        .await
        .unwrap();
    exchange(
        &mut tcp,
        Command::Request {
            bearer,
            invocation: Invocation {
                id: 1,
                operation: name.into(),
                input,
            },
        },
    )
    .await
}
async fn invoke(tcp: &mut TcpClient, name: &str, input: Value) -> Value {
    exchange(
        tcp,
        Command::Invoke(Invocation {
            id: 1,
            operation: name.into(),
            input,
        }),
    )
    .await
    .0
    .unwrap()
}
async fn exchange(
    tcp: &mut TcpClient,
    command: Command,
) -> (snap_transport::Outcome, Option<String>) {
    tcp.send(&command).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut bearer = None;
        loop {
            match tcp.receive().await.unwrap().0 {
                Response::Event(Event::Bearer {
                    change: Change::Set(token),
                    ..
                }) => bearer = Some(token.expose().into()),
                Response::Event(Event::Completed { outcome, .. }) => return (outcome, bearer),
                Response::Failed(error) => return (Err(error), bearer),
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}
fn seal(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let key = snap_config::MasterKey::generate().unwrap();
    let path = root.join("secrets.key");
    std::fs::write(&path, key.encode().expose()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(
        root.join("secrets.enc"),
        snap_config::Secrets::encrypt(
            b"oauth='fixture-client-secret-with-at-least-32-bytes'",
            &key,
        )
        .unwrap(),
    )
    .unwrap();
}
fn cli(root: &Path, authy: &str, chatty: &str, credential: &Value) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_chatty-agent"));
    command
        .args([
            "--authy",
            authy,
            "--addr",
            chatty,
            "--ca-file",
            root.join("ca.pem").to_str().unwrap(),
        ])
        .env("SNAP_AGENT_ID", credential["identity"].as_str().unwrap())
        .env("SNAP_AGENT_KEY", credential["key"].as_str().unwrap())
        .kill_on_drop(true);
    command
}
#[tokio::test]
#[ignore = "paired production Authy/Chatty binaries and real TCP/TLS CLI gate"]
async fn authy_agents_sign_in_post_watch_reconnect_and_lose_access_over_tcp() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let (_, tls) = pki::pki(root, false);
    let reserve = || std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ahttp = reserve();
    let atcp = reserve();
    let chttp = reserve();
    let ctcp = reserve();
    let abase = format!("http://{}", ahttp.local_addr().unwrap());
    let cbase = format!("http://{}", chttp.local_addr().unwrap());
    let aa = atcp.local_addr().unwrap().to_string();
    let ca = ctcp.local_addr().unwrap().to_string();
    let authy = root.join("authy");
    let chatty = root.join("chatty");
    for directory in [&authy, &chatty] {
        std::fs::create_dir(directory).unwrap();
        seal(directory);
        std::fs::create_dir(directory.join("web")).unwrap();
    }
    std::fs::write(
        authy.join("web/auth-pages.json"),
        r#"{"consent":"","logout":"","error":"","permission":""}"#,
    )
    .unwrap();
    let tcp_config = |addr: &str| {
        format!(
            "[app.tcp]\nlisten='{addr}'\ncert_file='{}'\nkey_file='{}'\n",
            root.join("server.pem").display(),
            root.join("server-key.pem").display()
        )
    };
    std::fs::write(authy.join("config.toml"),format!("version=1\n[host]\nmode='development'\nlisten='{}'\ndata_dir='data'\nweb_dir='web'\n[app]\nclients=[{{id='chatty',name='Chatty',origin='{cbase}',client_secret_ref='oauth'}},{{id='other',name='Other',origin='http://127.0.0.1:9999',client_secret_ref='oauth'}}]\n{}",ahttp.local_addr().unwrap(),tcp_config(&aa))).unwrap();
    std::fs::write(chatty.join("config.toml"),format!("version=1\n[host]\nmode='development'\nlisten='{}'\ndata_dir='data'\nweb_dir='web'\n[app.oauth]\nissuer='{abase}'\nclient_id='chatty'\nclient_secret_ref='oauth'\n{}",chttp.local_addr().unwrap(),tcp_config(&ca))).unwrap();
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().parent().unwrap();
    drop(ahttp);
    drop(atcp);
    let _authy = Host::start(
        &bin.join("authy"),
        &authy.join("config.toml"),
        &abase,
        root.join("authy.log"),
    )
    .await;
    drop(chttp);
    drop(ctcp);
    let mut chatty_host = Host::start(
        &bin.join("chatty"),
        &chatty.join("config.toml"),
        &cbase,
        root.join("chatty.log"),
    )
    .await;
    let mut sponsor = Requests {
        addr: aa.clone(),
        tls: tls.clone(),
    };
    let (_, human) = request(
        &mut sponsor,
        "identity.enroll",
        json!({"email":"sponsor@example.test","password":"fixture password"}),
        None,
    )
    .await;
    let human = human.unwrap();
    let owner = request(
        &mut sponsor,
        "authy.agent-create",
        json!({"name":"Owner agent"}),
        Some(human.clone()),
    )
    .await
    .0
    .unwrap();
    let helper = request(
        &mut sponsor,
        "authy.agent-create",
        json!({"name":"Helper"}),
        Some(human.clone()),
    )
    .await
    .0
    .unwrap();
    let principal = cli(root, &aa, &ca, &helper)
        .arg("identity")
        .output()
        .await
        .unwrap();
    assert!(
        principal.status.success(),
        "{}",
        String::from_utf8_lossy(&principal.stderr)
    );
    let principal: Value = serde_json::from_slice(&principal.stdout).unwrap();
    let local = principal["identity"].as_str().unwrap();
    assert_eq!(
        local,
        snap_identity::oauth::owner(&abase, helper["identity"].as_str().unwrap())
    );
    let assertion = request(
        &mut sponsor,
        "authy.agent-login",
        json!({"identity":owner["identity"],"key":owner["key"],"audience":"chatty"}),
        None,
    )
    .await
    .0
    .unwrap();
    let mut relying = Requests {
        addr: ca.clone(),
        tls: tls.clone(),
    };
    let mut forged = assertion["assertion"].as_str().unwrap().as_bytes().to_vec();
    let signature = forged.iter().rposition(|byte| *byte == b'.').unwrap() + 1;
    forged[signature] = if forged[signature] == b'A' {
        b'B'
    } else {
        b'A'
    };
    assert!(
        request(
            &mut relying,
            "identity.assertion-acquire",
            json!({"assertion":String::from_utf8(forged).unwrap()}),
            None
        )
        .await
        .0
        .is_err()
    );
    let (_, bearer) = request(
        &mut relying,
        "identity.assertion-acquire",
        json!({"assertion":assertion["assertion"]}),
        None,
    )
    .await;
    let mut control = TcpClient::open(&ca, &tls).await.unwrap();
    control
        .send(&Command::Connect {
            bearer: bearer.unwrap(),
            client_id: "control".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        control.receive().await.unwrap().0,
        Response::Attached { .. }
    ));
    invoke(
        &mut control,
        "chatty.create",
        json!({"id":THREAD,"title":"External clients"}),
    )
    .await;
    invoke(
        &mut control,
        "chatty.member",
        json!({"thread_id":THREAD,"identity":local,"role":"editor"}),
    )
    .await;
    let mut watch = cli(root, &aa, &ca, &helper)
        .args(["watch", THREAD])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(watch.stdout.take().unwrap()).lines();
    // Empty history first, then a new record is discovered by the live stream.
    invoke(
        &mut control,
        "chatty.send",
        json!({"thread_id":THREAD,"request_id":"owner-first","message":"Hello agent"}),
    )
    .await;
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(first["sequence"], 1);
    assert_eq!(first["body"], "Hello agent");
    let mut post = cli(root, &aa, &ca, &helper)
        .args(["post", THREAD, "--request", "helper-reply"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    post.stdin
        .take()
        .unwrap()
        .write_all(b"Hello human")
        .await
        .unwrap();
    let result = post.wait_with_output().await.unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let saved: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(saved["sender"], local);
    assert_eq!(saved["sequence"], 2);
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["body"],
        "Hello human"
    );
    watch.kill().await.unwrap();
    watch.wait().await.unwrap();
    drop(control);
    chatty_host.child.kill().unwrap();
    chatty_host.child.wait().unwrap();
    drop(chatty_host);
    let _restarted = Host::start(
        &bin.join("chatty"),
        &chatty.join("config.toml"),
        &cbase,
        root.join("chatty-restart.log"),
    )
    .await;
    let mut watch = cli(root, &aa, &ca, &helper)
        .args(["watch", THREAD, "--after", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(watch.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["sequence"], 2);
    // Same assertion cannot log into an unrelated audience, and key revocation
    // stops fresh app sessions without deleting the stable agent account.
    let wrong = cli(root, &aa, &ca, &helper)
        .args(["--audience", "other", "identity"])
        .output()
        .await
        .unwrap();
    assert!(!wrong.status.success());
    let (_, bearer) = request(
        &mut relying,
        "identity.assertion-acquire",
        json!({"assertion":assertion["assertion"]}),
        None,
    )
    .await;
    let mut control = TcpClient::open(&ca, &tls).await.unwrap();
    control
        .send(&Command::Connect {
            bearer: bearer.unwrap(),
            client_id: "revoke-control".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        control.receive().await.unwrap().0,
        Response::Attached { .. }
    ));
    invoke(
        &mut control,
        "chatty.member",
        json!({"thread_id":THREAD,"identity":local,"role":null}),
    )
    .await;
    let stopped = tokio::time::timeout(Duration::from_secs(10), watch.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !stopped.success(),
        "watch must stop when thread access is revoked"
    );
    request(
        &mut sponsor,
        "authy.agent-revoke",
        json!({"identity":helper["identity"]}),
        Some(human),
    )
    .await
    .0
    .unwrap();
    let denied = cli(root, &aa, &ca, &helper)
        .arg("identity")
        .output()
        .await
        .unwrap();
    assert!(!denied.status.success());
    assert!(denied.stdout.is_empty());
}
