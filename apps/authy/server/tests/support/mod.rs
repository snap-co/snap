use age::secrecy::ExposeSecret;
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use reqwest::{Client, RequestBuilder, Response};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const CLIENT_SECRET: &str = "fixture-client-secret-with-at-least-32-bytes";
pub const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
pub const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

struct Process {
    child: Child,
    readers: Vec<JoinHandle<()>>,
    lines: mpsc::Receiver<String>,
}
impl Process {
    fn start(mut command: Command) -> Self {
        use std::os::unix::process::CommandExt;
        let mut child = command
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (send, lines) = mpsc::channel();
        fn read(
            stream: impl std::io::Read + Send + 'static,
            send: mpsc::Sender<String>,
        ) -> JoinHandle<()> {
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let _ = send.send(line);
                }
            })
        }
        let readers = vec![
            read(child.stdout.take().unwrap(), send.clone()),
            read(child.stderr.take().unwrap(), send),
        ];
        Self {
            child,
            readers,
            lines,
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = killpg(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

pub struct Host {
    _directory: tempfile::TempDir,
    config: std::path::PathBuf,
    identity: age::x25519::Identity,
    process: Option<Process>,
    pub client: Client,
    pub base: String,
    pub origin: Option<String>,
    pub relying_party: String,
}
impl Host {
    pub async fn new(relying_party: &str, origin: Option<&str>) -> Self {
        let cache = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
            .join(".cache/coding-agents");
        fs::create_dir_all(&cache).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("authy-http-")
            .tempdir_in(cache)
            .unwrap();
        let identity = age::x25519::Identity::generate();
        fs::write(
            directory.path().join("secrets.enc"),
            snap_config::Secrets::encrypt(
                format!("[clients]\nchatty='{CLIENT_SECRET}'\n").as_bytes(),
                &[identity.to_public()],
            )
            .unwrap(),
        )
        .unwrap();
        let mut host = Self {
            config: directory.path().join("config.toml"),
            _directory: directory,
            identity,
            process: None,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            base: String::new(),
            origin: origin.map(str::to_owned),
            relying_party: relying_party.into(),
        };
        host.configure();
        let migrated = host.command().arg("--migrate").output().unwrap();
        assert!(
            migrated.status.success(),
            "{}",
            String::from_utf8_lossy(&migrated.stderr)
        );
        host.start().await;
        host
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_authy"));
        command
            .args(["--config"])
            .arg(&self.config)
            .env("SNAP_MASTER_KEY", self.identity.to_string().expose_secret());
        command
    }

    fn configure(&self) {
        let listen = if self.base.is_empty() {
            "127.0.0.1:0"
        } else {
            self.base.strip_prefix("http://").unwrap()
        };
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../dist/development/clients/web");
        assert!(
            assets.join("auth-pages.json").is_file(),
            "Prepare Authy assets with snap build --project apps/authy --web-only --output apps/authy/dist/development/clients/web"
        );
        let mut host = json!({"mode":"development", "listen":listen, "data_dir":"data", "database":"authy.sqlite", "web_dir":assets});
        if let Some(origin) = &self.origin {
            host["origin"] = json!(origin);
        }
        let config = json!({"version":1,"host":host,"app":{"auto_approve_domain":"snapco.dev","clients":[{"id":"chatty","name":"Chatty","origin":self.relying_party,"client_secret_ref":"clients.chatty"}]}});
        fs::write(&self.config, toml::to_string(&config).unwrap()).unwrap();
    }

    async fn start(&mut self) {
        self.configure();
        self.process = Some(Process::start(self.command()));
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut logs = String::new();
        loop {
            let process = self.process.as_mut().unwrap();
            for line in process.lines.try_iter() {
                if let Some(address) = line.strip_prefix("Authy http://") {
                    self.base = format!("http://{address}");
                }
                logs.push_str(&line);
                logs.push('\n');
            }
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "Authy exited: {logs}"
            );
            if !self.base.is_empty()
                && self
                    .client
                    .get(format!("{}/health", self.base))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            assert!(Instant::now() < deadline, "Authy startup timed out: {logs}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn restart(&mut self) {
        drop(self.process.take());
        self.start().await;
    }

    pub fn request(&self, path: &str, cookie: &str) -> RequestBuilder {
        self.client
            .get(format!("{}{path}", self.base))
            .header("cookie", cookie)
    }

    pub fn post(&self, path: &str, value: &Value, cookie: &str, origin: &str) -> RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("cookie", cookie)
            .header("origin", origin)
            .json(value)
    }

    pub fn form(&self, path: &str, values: &[(&str, &str)], cookie: &str) -> RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("cookie", cookie)
            .header("origin", self.public_origin())
            .form(values)
    }

    pub fn public_origin(&self) -> &str {
        self.origin.as_deref().unwrap_or(&self.base)
    }

    pub async fn login(&self, signup: bool, email: &str, password: &str) -> (String, Value) {
        let response = self
            .post(
                if signup {
                    "/identity/enroll"
                } else {
                    "/identity/acquire"
                },
                &json!({"email":email,"password":password}),
                "",
                self.public_origin(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let cookie = cookie(&response);
        let principal =
            response.json::<Value>().await.unwrap()["Completed"]["outcome"]["Ok"].clone();
        let account = value(
            self.request("/authy/account", &cookie)
                .send()
                .await
                .unwrap(),
        )
        .await["Completed"]["outcome"]["Ok"]
            .clone();
        assert_eq!(principal["identity"], account["identity"]);
        (cookie, account)
    }

    pub async fn invoke(
        &self,
        cookie: &str,
        operation: &str,
        input: Value,
    ) -> Result<Value, Value> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
        let run = async {
            let mut request = format!("{}/transport", self.base.replace("http://", "ws://"))
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("cookie", cookie.parse().unwrap());
            request
                .headers_mut()
                .insert("origin", self.public_origin().parse().unwrap());
            let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
            static CLIENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let client_id = format!(
                "rust-http-test-{}",
                CLIENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            socket
                .send(Message::Text(
                    json!({"Connect":{"bearer":"","client_id":client_id}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let mut accepted = false;
            while let Some(message) = socket.next().await {
                let message = message.unwrap();
                if !message.is_text() {
                    continue;
                }
                let frame: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if !frame["Attached"].is_null() {
                    socket
                        .send(Message::Text(
                            json!({"Invoke":{"id":1,"operation":operation,"input":input}})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                }
                if !frame["Failed"].is_null() {
                    return Err(frame["Failed"].clone());
                }
                for event in frame["Events"].as_array().into_iter().flatten() {
                    if !event["Accepted"].is_null() {
                        accepted = true;
                    }
                    if !event["Completed"].is_null() {
                        let outcome = &event["Completed"]["outcome"];
                        socket.close(None).await.unwrap();
                        if let Some(error) = outcome.get("Err") {
                            return Err(error.clone());
                        }
                        assert!(accepted, "Completion preceded acceptance");
                        return Ok(outcome["Ok"].clone());
                    }
                }
            }
            panic!("WebSocket closed before completion");
        };
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("Invocation timed out")
    }
}

pub fn cookie(response: &Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}
pub async fn value(response: Response) -> Value {
    response.json().await.unwrap()
}
pub fn destination(response: &Response) -> url::Url {
    url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap()
}
pub fn query(url: &url::Url, key: &str) -> String {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .unwrap()
        .1
        .into_owned()
}
