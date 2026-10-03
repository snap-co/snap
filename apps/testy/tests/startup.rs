use std::{
    fs,
    io::{BufRead, BufReader},
    net::TcpStream,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "native startup process boundary"]
fn native_startup_validates_app_schema_and_present_bags_before_listening() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    let data = directory.path().join("data");
    fs::create_dir(&data).unwrap();
    let text = format!(
        "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='{}'\n[app]\n",
        data.display()
    );
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_testy-server-native"));
        command
            .args(["--config", config.to_str().unwrap()])
            .env_remove("SNAP_MASTER_KEY");
        command
    };
    fs::write(&config, format!("{text}unknown=true\n")).unwrap();
    let check = command().arg("--check-config").output().unwrap();
    assert!(!check.status.success());
    assert!(String::from_utf8_lossy(&check.stderr).contains("Invalid config.toml schema"));
    fs::write(&config, &text).unwrap();
    assert!(command().arg("--check-config").status().unwrap().success());
    snap_store_sqlite::migrate(
        &data.join("store.sqlite"),
        &[
            toml::from_str(snap_identity::MIGRATION).unwrap(),
            toml::from_str(snap_identity::SESSION_TIME_MIGRATION).unwrap(),
            toml::from_str(snap_identity::CREDENTIAL_KIND_MIGRATION).unwrap(),
            toml::from_str(snap_identity::FLOW_MIGRATION).unwrap(),
        ],
    )
    .unwrap();
    fs::write(directory.path().join("secrets.enc"), b"damaged bag").unwrap();
    let mut rejected = Server(
        command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = rejected.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "Invalid bag did not stop native startup"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let output = rejected.0.stdout.take().unwrap();
    assert_eq!(
        BufReader::new(output).lines().count(),
        0,
        "Invalid bag must fail before listener readiness"
    );
    fs::remove_file(directory.path().join("secrets.enc")).unwrap();
    let mut server = Server(command().stdout(Stdio::piped()).spawn().unwrap());
    let stdout = server.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = send.send(BufReader::new(stdout).read_line(&mut line).map(|_| line));
    });
    let line = receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let address = line.trim().strip_prefix("Testy listening on ").unwrap();
    TcpStream::connect(address).unwrap();
}
