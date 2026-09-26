use std::{fs, process::Command};

#[test]
fn migration_command_creates_applies_and_detects_edited_history() {
    let root = std::env::temp_dir().join(format!("snap-migrate-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap()
    };
    let created = cli(&["migrate", "new", "accounts"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let file = fs::read_dir(root.join("migrations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let id = file.file_stem().unwrap().to_str().unwrap();
    fs::write(
        &file,
        format!(
            r#"id = "{id}"
[[changes]]
action = "create_table"
[changes.table]
name = "identity.accounts"
columns = [{{name = "id", kind = "integer"}}]
primary = ["id"]
"#
        ),
    )
    .unwrap();
    for _ in 0..2 {
        let result = cli(&["migrate", "--database", "test.sqlite"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(
        cli(&["migrate", "--database", "test.sqlite", "--status"])
            .status
            .success()
    );
    fs::write(
        &file,
        fs::read_to_string(&file)
            .unwrap()
            .replace("identity.accounts", "identity.people"),
    )
    .unwrap();
    let result = cli(&["migrate", "--database", "test.sqlite"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("changed"));
    fs::remove_dir_all(root).unwrap();
}
