use std::{fs, process::Command};

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
    fs::write(&config, format!(
        "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n\
         [app.oauth]\nissuer='http://127.0.0.1:1'\nclient_id='factorio'\nclient_secret_ref='oauth.client_secret'\n\
         [app.tcp]\nlisten='127.0.0.1:0'\ncert_file='missing.pem'\nkey_file='missing-key.pem'\n\
         [app.repository]\nrepository='{}'\nmainline='main'\nmodules={{}}\nresources='{}'\nfirst_port=15000\nsetup=[]\nteardown=[]\n",
        root.path().join("repository").display(), root.path().join("resources").display()
    )).unwrap();
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
