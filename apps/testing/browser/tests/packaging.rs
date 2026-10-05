//! Testy's production host owns origin policy and the absence of debugger routes.
use std::{fs, process::Command};

#[test]
#[ignore = "native production packaging; build snap-cli first"]
fn production_package_serves_without_publishing_testy_debugger_controls() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpStream,
        process::Stdio,
    };
    // The native route builds snap-cli alongside this Cargo test executable,
    // including when Cargo selects a nondefault target directory or target triple.
    let executable = std::env::current_exe().unwrap();
    let cli = executable.parent().unwrap().parent().unwrap().join("snap");
    let source = snap_app_browser_tests::support::SourceCopy::new().unwrap();
    let root = source.root.join("apps/testy");
    let input = root.join(".deployment/production");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("config.toml"), format!("version=1\n[host]\nmode='production'\nlisten='0.0.0.0:0'\norigin='https://testy.example.test:443/'\ndata_dir='{}'\n[app]\n", source.path().join("data").display())).unwrap();
    let result = Command::new(cli)
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", source.path().join("gate-target"))
        .args(["build", "production"])
        .env_remove("SNAP_MASTER_KEY")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let package = root.join("dist/production");
    assert!(!package.join("clients/web/app.js.map").exists());
    assert!(package.join("clients/web/app.js").is_file());
    assert!(
        package
            .join("clients/web/bindings/testy_wasm.d.ts")
            .is_file()
    );
    let artifacts: snap_config::Artifacts =
        toml::from_str(&fs::read_to_string(package.join("artifacts.toml")).unwrap()).unwrap();
    assert_eq!(artifacts.native_clients.len(), 1);
    assert!(
        package
            .join(&artifacts.native_clients[0].executable)
            .is_file()
    );
    let executable = package.join(&artifacts.servers[0].executable);
    assert!(
        Command::new(&executable)
            .arg("--migrate")
            .status()
            .unwrap()
            .success()
    );
    let mut server = Command::new(&executable)
        .env_remove("SNAP_MASTER_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    struct Stop<'a>(&'a mut std::process::Child);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let guard = Stop(&mut server);
    let stdout = guard.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = send.send(BufReader::new(stdout).read_line(&mut line).map(|_| line));
    });
    let line = receive
        .recv_timeout(std::time::Duration::from_secs(15))
        .unwrap()
        .unwrap();
    let address: std::net::SocketAddr = line
        .trim()
        .strip_prefix("Testy http://")
        .expect("server readiness")
        .parse()
        .unwrap();
    for (origin, status) in [
        ("https://testy.example.test", "101"),
        ("https://foreign.example.test", "403"),
    ] {
        let mut stream = TcpStream::connect(("127.0.0.1", address.port())).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write!(stream, "GET /transport HTTP/1.1\r\nHost: testy.example.test\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{response}"
        );
    }
    for path in ["/__dev", "/__dev/ws"] {
        let mut stream = TcpStream::connect(("127.0.0.1", address.port())).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: testy.example.test\r\nOrigin: https://testy.example.test\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    }
}
