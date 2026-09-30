use std::{fs, path::Path, process::Command};

fn configuration(root: &Path) -> String {
    format!(
        "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n\
         [app.oauth]\nissuer='http://127.0.0.1:1'\nclient_id='factorio'\nclient_secret_ref='oauth.client_secret'\n\
         [app.tcp]\nlisten='127.0.0.1:0'\ncert_file='missing.pem'\nkey_file='missing-key.pem'\n\
         [app.repository]\nrepository='{}'\nmainline='main'\nmodules={{}}\nresources='{}'\nfirst_port=15000\nsetup=[]\nteardown=[]\n",
        root.join("repository").display(),
        root.join("resources").display()
    )
}

#[test]
fn one_executable_exposes_client_and_server_commands_without_a_login() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["--help"],
        vec!["serve", "--help"],
        vec!["status", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_factorio"))
            .env_clear()
            .env("HOME", home.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("factorio"));
    }
    assert!(!home.path().join(".config").exists());
}

#[test]
fn server_preparation_is_offline_and_separate_from_client_credentials() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config, configuration(root.path())).unwrap();
    // Checking only requires the bag to exist, never its decryption key or TLS files.
    fs::write(root.path().join("secrets.enc"), []).unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_factorio"))
            .env_clear()
            .env("HOME", root.path().join("home"))
            .args(args)
            .arg("--config")
            .arg(&config)
            .output()
            .unwrap()
    };
    let check = invoke(&["serve", "--check-config"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(!root.path().join("data").exists());
    let conflict = invoke(&["serve", "--check-config", "--migrate"]);
    assert!(!conflict.status.success());
    assert!(!root.path().join("data").exists());
    let migrate = invoke(&["serve", "--migrate"]);
    assert!(
        migrate.status.success(),
        "{}",
        String::from_utf8_lossy(&migrate.stderr)
    );
    let database = root.path().join("data/store.sqlite");
    assert!(database.is_file());
    let mut store = snap_sqlite::Sqlite::open(&database).unwrap();
    store.load("factorio.cli").unwrap();
    assert!(!root.path().join("home").exists());
}

#[test]
fn checkout_server_discovers_its_private_profile_but_preserves_explicit_and_packaged_config() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let application = checkout.join("apps/factorio");
    let private = application.join(".snap/development");
    let template = application.join(".deployment/development");
    let binary_dir = checkout.join("target/debug");
    for directory in [&private, &template, &binary_dir] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(
        application.join("snap.toml"),
        "version=1\napplication='factorio'\n",
    )
    .unwrap();
    let binary = binary_dir.join(format!("factorio{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_factorio"), &binary).unwrap();
    let text = configuration(root.path());
    fs::write(private.join("config.toml"), &text).unwrap();
    fs::write(template.join("config.toml"), &text).unwrap();
    let invoke = |args: &[&str]| {
        Command::new(&binary)
            .env_clear()
            .current_dir(root.path())
            .args(args)
            .output()
            .unwrap()
    };
    // Executable ancestry finds the checkout even outside its working directory.
    let migrated = invoke(&["serve", "--migrate"]);
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(private.join("data/store.sqlite").is_file());
    assert!(!template.join("data").exists());
    fs::write(private.join("config.toml"), "private-invalid-profile").unwrap();
    let rejected = invoke(&["serve", "--migrate"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("Invalid config.toml schema"));
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains("private-invalid-profile"));
    assert!(!template.join("data").exists());
    let explicit = Command::new(&binary)
        .env_clear()
        .current_dir(root.path())
        .args(["serve", "--migrate", "--config"])
        .arg(template.join("config.toml"))
        .output()
        .unwrap();
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(template.join("data/store.sqlite").is_file());
    // A deployed executable's adjacent config always wins over checkout discovery.
    fs::write(binary_dir.join("config.toml"), &text).unwrap();
    let packaged = invoke(&["serve", "--migrate"]);
    assert!(
        packaged.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged.stderr)
    );
    assert!(binary_dir.join("data/store.sqlite").is_file());
    // Without a private profile, the tracked development template remains usable.
    fs::remove_file(binary_dir.join("config.toml")).unwrap();
    fs::remove_file(private.join("config.toml")).unwrap();
    let fallback = invoke(&["serve", "--migrate"]);
    assert!(
        fallback.status.success(),
        "{}",
        String::from_utf8_lossy(&fallback.stderr)
    );
}

#[test]
fn development_server_reads_a_private_key_but_production_requires_an_explicit_key() {
    use age::secrecy::ExposeSecret;
    let root = tempfile::tempdir().unwrap();
    let identity = age::x25519::Identity::generate();
    let key = root.path().join("secrets.key");
    fs::write(&key, identity.to_string().expose_secret()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(
        root.path().join("secrets.enc"),
        snap_config::Secrets::encrypt(
            b"[oauth]\nclient_secret='private-native-defaults-credential'\n",
            &[identity.to_public()],
        )
        .unwrap(),
    )
    .unwrap();
    // This distinct TLS failure proves decryption finished without starting any listener.
    fs::write(root.path().join("missing.pem"), "not a TLS certificate").unwrap();
    let path = root.path().join("config.toml");
    let run = |explicit: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_factorio"));
        command.env_clear().args(["serve", "--config"]).arg(&path);
        if let Some(key) = explicit {
            command.env("SNAP_MASTER_KEY", key);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(!error.contains("private-native-defaults-credential"));
        assert!(!error.contains(identity.to_string().expose_secret()));
        error
    };
    fs::write(&path, configuration(root.path())).unwrap();
    assert!(run(None).contains("PEM file contains no certificates"));
    let wrong = age::x25519::Identity::generate();
    assert!(
        !run(Some(wrong.to_string().expose_secret())).contains("PEM file contains no certificates")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(run(None).contains("secrets.key must be private"));
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(
        &path,
        configuration(root.path())
            .replace(
                "mode='development'",
                "mode='production'\norigin='https://factorio.example.test'",
            )
            .replace(
                "data_dir='data'",
                &format!("data_dir='{}'", root.path().join("data").display()),
            ),
    )
    .unwrap();
    let production_error = run(None);
    assert!(
        production_error.contains("SNAP_MASTER_KEY is required"),
        "{production_error}"
    );
    assert!(
        run(Some(identity.to_string().expose_secret()))
            .contains("PEM file contains no certificates")
    );
}

#[test]
fn fresh_checkout_login_uses_the_server_default_when_tcp_listen_is_omitted() {
    let root = tempfile::tempdir().unwrap();
    let application = root.path().join("apps/factorio");
    let profile = application.join(".snap/development");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        application.join("snap.toml"),
        "version=1\napplication='factorio'\n",
    )
    .unwrap();
    let text = configuration(root.path()).replace(
        "listen='127.0.0.1:0'\ncert_file='missing.pem'",
        "ca_file='ca.pem'\ncert_file='missing.pem'",
    );
    fs::write(profile.join("config.toml"), text).unwrap();
    fs::write(profile.join("secrets.enc"), []).unwrap();
    // Reach local trust validation without connecting to the default port, which
    // may belong to an operator's real installation.
    fs::write(profile.join("ca.pem"), "not a certificate").unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_factorio"))
            .env_clear()
            .current_dir(root.path())
            .args(args)
            .output()
            .unwrap()
    };
    let check = invoke(&["serve", "--check-config"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let credentials = root.path().join("credentials.json");
    let login = invoke(&["login", "--credentials", credentials.to_str().unwrap()]);
    assert!(!login.status.success());
    let error = String::from_utf8(login.stderr).unwrap();
    assert!(
        error.contains("PEM file contains no certificates"),
        "{error}"
    );
}
