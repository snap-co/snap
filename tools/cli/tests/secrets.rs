use serde::Deserialize;
use std::{fs, process::Command};

#[derive(Deserialize)]
struct Application {
    secret_ref: snap_config::SecretRef,
}

#[test]
fn development_secrets_are_initialized_and_sealed_in_the_selected_private_profile() {
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
    let selected = root.path().join(".snap/development");
    let template = root.path().join(".deployment/development");
    fs::create_dir_all(&selected).unwrap();
    fs::create_dir_all(&template).unwrap();
    let text = "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n[app]\nsecret_ref='oauth.client_secret'\n";
    fs::write(selected.join("config.toml"), text).unwrap();
    fs::write(template.join("config.toml"), text).unwrap();
    let run = |action: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(root.path())
            .env_remove("SNAP_MASTER_KEY")
            .args(["secrets", action])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-profile-client-secret"));
    };
    run("init");
    fs::write(
        selected.join("secrets.toml"),
        "[oauth]\nclient_secret='private-profile-client-secret'\n",
    )
    .unwrap();
    run("seal");
    let identity = fs::read_to_string(selected.join("secrets.key"))
        .unwrap()
        .parse::<age::x25519::Identity>()
        .unwrap();
    let config = snap_config::Config::<Application>::read(&selected.join("config.toml")).unwrap();
    assert_eq!(
        config
            .secrets(Some(&identity))
            .unwrap()
            .resolve(&config.app.secret_ref)
            .unwrap()
            .expose(),
        "private-profile-client-secret"
    );
    assert_eq!(
        fs::read_to_string(template.join("config.toml")).unwrap(),
        text
    );
    assert!(!template.join("secrets.key").exists());
    assert!(!template.join("secrets.enc").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(selected.join("secrets.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
}
