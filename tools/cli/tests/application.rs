use std::{fs, process::Command};

#[test]
fn build_selects_the_named_environment_without_development_fallback() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    fs::write(
        root.path().join("snap.toml"),
        "version=1\napplication='fixture'\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join(".deployment/development")).unwrap();
    fs::write(
        root.path().join(".deployment/development/config.toml"),
        "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n[app]\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join("dist/production")).unwrap();
    fs::write(root.path().join("dist/production/previous"), "retained").unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.path())
        .args(["build", "production"])
        .output()
        .unwrap();
    assert!(!build.status.success());
    assert!(String::from_utf8_lossy(&build.stderr).contains("config.toml"));
    assert_eq!(
        fs::read_to_string(root.path().join("dist/production/previous")).unwrap(),
        "retained"
    );
    let traversal = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.path())
        .args(["build", "../production"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&traversal.stderr).contains("Invalid environment"));
    let package = root.path().join("dist/development");
    fs::create_dir_all(package.join("data")).unwrap();
    fs::write(package.join("previous"), "retained").unwrap();
    let alias = root.path().join("storage-alias");
    std::os::unix::fs::symlink(package.join("data"), &alias).unwrap();
    fs::create_dir_all(root.path().join(".deployment/production")).unwrap();
    for (environment, data) in [
        ("development", "data".to_owned()),
        (
            "production",
            package.join("data").to_string_lossy().into_owned(),
        ),
        ("production", alias.to_string_lossy().into_owned()),
    ] {
        fs::write(root.path().join(".deployment").join(environment).join("config.toml"), format!("version=1\n[host]\nmode='{environment}'\nlisten='127.0.0.1:0'\norigin='https://fixture.example.test'\ndata_dir='{data}'\n[app]\n")).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(root.path())
            .args(["build", environment])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("Persistent data must be outside dist"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            fs::read_to_string(package.join("previous")).unwrap(),
            "retained"
        );
    }
    let external = root.path().join("external-release");
    fs::rename(&package, &external).unwrap();
    fs::write(
        root.path().join(".deployment/development/config.toml"),
        "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n[app]\n",
    )
    .unwrap();
    for target in [external.clone(), root.path().join("missing-release")] {
        std::os::unix::fs::symlink(&target, &package).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(root.path())
            .arg("build")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("Package output must not be a symlink"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read_link(&package).unwrap(), target);
        assert_eq!(
            fs::read_to_string(external.join("previous")).unwrap(),
            "retained"
        );
        fs::remove_file(&package).unwrap();
    }
}

#[test]
#[ignore = "native packaging"]
fn native_package_is_relocatable_and_excludes_private_deployment_files() {
    use age::secrecy::ExposeSecret;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpStream,
        process::Stdio,
    };
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    for name in ["server", "web"] {
        std::os::unix::fs::symlink(
            repository.join("apps/chatty").join(name),
            root.path().join(name),
        )
        .unwrap();
    }
    std::os::unix::fs::symlink(
        repository.join("node_modules"),
        root.path().join("node_modules"),
    )
    .unwrap();
    fs::copy(
        repository.join("apps/chatty/Cargo.toml"),
        root.path().join("Cargo.toml"),
    )
    .unwrap();
    let settings = fs::read_to_string(repository.join("apps/chatty/snap.toml")).unwrap();
    fs::write(root.path().join("snap.toml"), format!("{settings}\n[build.clients.preview]\nkind='web'\nsource='web'\nwasm='web/wasm/Cargo.toml'\n[build.servers.replica]\nmanifest='server/Cargo.toml'\ntargets=['host']\n")).unwrap();
    let input = root.path().join(".deployment/development");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("config.toml"), format!("version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='{}'\n[app.oauth]\nissuer='http://127.0.0.1:3846'\nclient_id='chatty'\nclient_secret_ref='oauth.client_secret'\n", root.path().join("data").display())).unwrap();
    let identity = age::x25519::Identity::generate();
    let secret = "packaging-test-client-credential-at-least-32-bytes";
    fs::write(
        input.join("secrets.key"),
        identity.to_string().expose_secret(),
    )
    .unwrap();
    fs::write(input.join("secrets.toml"), secret).unwrap();
    fs::write(input.join("private-notes.txt"), "not packaged").unwrap();
    fs::write(
        input.join("secrets.enc"),
        snap_config::Secrets::encrypt(
            format!("[oauth]\nclient_secret='{secret}'\n").as_bytes(),
            &[identity.to_public()],
        )
        .unwrap(),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.path())
        .arg("build")
        .env_remove("SNAP_MASTER_KEY")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let package = root.path().join("dist/development");
    for name in [
        "secrets.key",
        "secrets.toml",
        "private-notes.txt",
        "web-build.mjs",
    ] {
        assert!(!package.join(name).exists());
    }
    assert!(package.join("clients/web/index.html").is_file());
    assert!(
        package
            .join("clients/web/bindings/chatty_wasm_bg.wasm")
            .is_file()
    );
    assert!(package.join("clients/web/client.js").is_file());
    let artifacts: snap_config::Artifacts =
        toml::from_str(&fs::read_to_string(package.join("artifacts.toml")).unwrap()).unwrap();
    assert_eq!(artifacts.servers.len(), 2);
    assert_eq!(artifacts.clients.len(), 2);
    assert!(package.join("clients/preview/index.html").is_file());
    for artifact in &artifacts.servers {
        assert!(package.join(&artifact.executable).is_file());
        assert_ne!(artifact.target, "host");
    }
    let inventory = fs::read_to_string(package.join("artifacts.toml")).unwrap();
    for (targets, expected) in [
        (vec!["--target", "not-a-rust-target"], "Unknown Rust target"),
        (
            vec!["--target", "host", "--target", "host"],
            "Duplicate native target",
        ),
        (
            vec!["--target", "wasm32-unknown-unknown"],
            "runtime adapter",
        ),
    ] {
        let rejected = Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(root.path())
            .arg("build")
            .args(targets)
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains(expected));
        assert_eq!(
            fs::read_to_string(package.join("artifacts.toml")).unwrap(),
            inventory
        );
    }
    let selected = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.path())
        .args([
            "build",
            "--server",
            "native",
            "--target",
            &artifacts.servers[0].target,
        ])
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let artifacts: snap_config::Artifacts =
        toml::from_str(&fs::read_to_string(package.join("artifacts.toml")).unwrap()).unwrap();
    assert_eq!(artifacts.servers.len(), 1);
    assert_eq!(artifacts.servers[0].name, "native");
    assert!(!package.join("servers/replica").exists());
    let executable = &artifacts.servers[0].executable;
    let relocated = root.path().join("relocated");
    fs::rename(package, &relocated).unwrap();
    let migrate = Command::new(relocated.join(executable))
        .current_dir(&repository)
        .arg("--migrate")
        .output()
        .unwrap();
    assert!(
        migrate.status.success(),
        "{}",
        String::from_utf8_lossy(&migrate.stderr)
    );
    let mut server = Command::new(relocated.join(executable))
        .current_dir(&repository)
        .env("SNAP_MASTER_KEY", identity.to_string().expose_secret())
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
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    let line = receive
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("bounded readiness")
        .unwrap();
    let address = line
        .trim()
        .strip_prefix("Chatty http://")
        .expect("server readiness");
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
}

#[test]
#[ignore = "native production packaging"]
fn production_package_serves_without_publishing_testy_debugger_controls() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpStream,
        process::Stdio,
    };
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    for name in ["server", "native", "web"] {
        std::os::unix::fs::symlink(
            repository.join("apps/testy").join(name),
            root.path().join(name),
        )
        .unwrap();
    }
    std::os::unix::fs::symlink(
        repository.join("node_modules"),
        root.path().join("node_modules"),
    )
    .unwrap();
    fs::copy(
        repository.join("apps/testy/Cargo.toml"),
        root.path().join("Cargo.toml"),
    )
    .unwrap();
    fs::copy(
        repository.join("apps/testy/snap.toml"),
        root.path().join("snap.toml"),
    )
    .unwrap();
    let input = root.path().join(".deployment/production");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("config.toml"), format!("version=1\n[host]\nmode='production'\nlisten='0.0.0.0:0'\norigin='https://testy.example.test:443/'\ndata_dir='{}'\n[app]\n", root.path().join("data").display())).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.path())
        .args(["build", "production"])
        .env_remove("SNAP_MASTER_KEY")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let package = root.path().join("dist/production");
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

#[test]
fn development_rejects_obsolete_app_commands_without_executing_them() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    fs::write(
        root.join("snap.toml"),
        r#"
version = 1
application = "fixture"
[dev]
commands = [["sh", "-c", "pwd > built; exit 17"], ["touch", "should-not-run"]]
"#,
    )
    .unwrap();
    let dev = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.join("nested"))
        .arg("dev")
        .output()
        .unwrap();
    assert!(!dev.status.success());
    assert!(String::from_utf8_lossy(&dev.stderr).contains("Invalid"));
    assert!(!root.join("built").exists());
    assert!(!root.join("should-not-run").exists());
}
