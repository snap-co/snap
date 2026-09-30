use std::{fs, process::Command};

#[test]
fn dev_validates_installation_and_options_before_starting_processes() {
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
    let deployment = root.path().join(".deployment/development");
    fs::create_dir_all(&deployment).unwrap();
    let nested = root.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let config = deployment.join("config.toml");
    for (contents, arguments, expected) in [
        (None, vec!["dev"], "config.toml"),
        (None, vec!["dev", ".."], "config.toml"),
        (
            None,
            vec!["dev", "missing-project"],
            "Project directory does not exist",
        ),
        (None, vec!["dev", "--config", "absent.toml"], "config.toml"),
        (
            Some("invalid private configuration that must not be echoed"),
            vec!["dev"],
            "Invalid config.toml schema",
        ),
        (
            Some(
                "version=1\n[host]\nmode='production'\nlisten='127.0.0.1:0'\norigin='https://fixture.example.test'\ndata_dir='/data'\n[app]\n",
            ),
            vec!["dev"],
            "requires development configuration",
        ),
        (
            Some(
                "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\norigin='https://fixture.example.test'\ndata_dir='data'\n[dev]\nlisten='0.0.0.0:3852'\n[app]\n",
            ),
            vec!["dev"],
            "HTTPS development requires a loopback frontend",
        ),
        (None, vec!["dev", "--unknown"], "unexpected argument"),
    ] {
        if let Some(contents) = contents {
            fs::write(&config, contents).unwrap();
        } else if config.exists() {
            fs::remove_file(&config).unwrap();
        }
        let result = Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(&nested)
            .env_remove("SNAP_MASTER_KEY")
            .args(&arguments)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{arguments:?}");
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(!error.contains("private configuration"));
        assert!(
            error.contains(expected),
            "{arguments:?}: expected {expected}, got {error}"
        );
        assert!(
            !root.path().join(".snap").exists(),
            "Invalid inputs must not start a development generation"
        );
    }
    fs::write(
        root.path().join("snap.toml"),
        "version=1\napplication='testy'\n",
    )
    .unwrap();
    fs::write(&config, "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n[dev]\nlisten='0.0.0.0:0'\n[app]\n").unwrap();
    let testy = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(&nested)
        .arg("dev")
        .output()
        .unwrap();
    assert!(!testy.status.success());
    assert!(
        String::from_utf8_lossy(&testy.stderr)
            .contains("Testy development requires a loopback frontend")
    );
    assert!(!root.path().join(".snap").exists());
}

#[test]
fn dev_prefers_the_private_profile_and_explicit_config_overrides_it() {
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
    let private = root.path().join(".snap/development");
    let template = root.path().join(".deployment/development");
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(&template).unwrap();
    fs::write(private.join("config.toml"), "invalid-private-profile").unwrap();
    fs::write(template.join("config.toml"), "version=1\n[host]\nmode='production'\nlisten='127.0.0.1:0'\norigin='https://fixture.example.test'\ndata_dir='/data'\n[app]\n").unwrap();
    let invoke = |explicit: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_snap"));
        command
            .current_dir(root.path())
            .env_remove("SNAP_MASTER_KEY")
            .arg("dev");
        if explicit {
            command.arg("--config").arg(template.join("config.toml"));
        }
        let result = command.output().unwrap();
        assert!(!result.status.success());
        String::from_utf8(result.stderr).unwrap()
    };
    let selected = invoke(false);
    assert!(
        selected.contains("Invalid config.toml schema"),
        "{selected}"
    );
    assert!(!selected.contains("invalid-private-profile"));
    let explicit = invoke(true);
    assert!(
        explicit.contains("requires development configuration"),
        "{explicit}"
    );
}
