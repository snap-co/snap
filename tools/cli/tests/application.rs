use std::{fs, process::Command};

#[test]
fn app_workflows_run_from_the_project_and_propagate_failure() {
    let root = std::env::temp_dir().join(format!("snap-workflow-{}", std::process::id()));
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
[build]
commands = [["sh", "-c", "pwd > built"]]
[dev]
commands = [["sh", "-c", "test -f built && exit 17"], ["touch", "should-not-run"]]
"#,
    )
    .unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_snap"))
        .current_dir(root.join("nested"))
        .arg("build")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("built")).unwrap().trim(),
        root.to_str().unwrap()
    );
    let dev = Command::new(env!("CARGO_BIN_EXE_snap"))
        .args(["dev", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(dev.status.code(), Some(17));
    assert!(!root.join("should-not-run").exists());
    fs::remove_dir_all(root).unwrap();
}
