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

#[test]
fn generated_successor_extends_a_numbered_migration_history() {
    let root = std::env::temp_dir().join(format!("snap-migrate-successor-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("migrations")).unwrap();
    fs::write(
        root.join("migrations/0001_accounts.toml"),
        r#"id = "0001_accounts"
[[changes]]
action = "create_table"
[changes.table]
name = "identity.accounts"
columns = [{name = "id", kind = "integer"}]
primary = ["id"]
"#,
    )
    .unwrap();
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(
        cli(&["migrate", "--database", "store.sqlite"])
            .status
            .success()
    );
    let created = cli(&["migrate", "new", "email"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let path = fs::read_dir(root.join("migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_stem().unwrap() != "0001_accounts")
        .unwrap();
    let id = path.file_stem().unwrap().to_str().unwrap();
    fs::write(
        &path,
        format!(
            r#"id = "{id}"
[[changes]]
action = "add_column"
table = "identity.accounts"
column = {{ name = "email", kind = "text" }}
fill = ""
"#
        ),
    )
    .unwrap();
    let result = cli(&["migrate", "--database", "store.sqlite"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        cli(&["migrate", "--database", "store.sqlite"])
            .status
            .success()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn identity_directory_initializes_once_and_preserves_rows_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let migrations =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/identity/migrations");
    let apply = |database: &std::path::Path, sources: &std::path::Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_snap"))
            .current_dir(directory.path())
            .arg("migrate")
            .arg("--database")
            .arg(database)
            .arg("--migrations")
            .arg(sources)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let database = directory.path().join("fresh.sqlite");
    apply(&database, &migrations);
    let credential = snap_store::Row::from([
        ("locator".into(), "person@example.test".into()),
        ("identity".into(), "owner".into()),
        ("material".into(), "password-hash".into()),
        ("kind".into(), "password".into()),
        ("label".into(), "Password".into()),
    ]);
    let session = snap_store::Row::from([
        ("digest".into(), snap_store::Value::Bytes(vec![1; 32])),
        ("identity".into(), "owner".into()),
        ("expires".into(), 1000.into()),
        ("issued".into(), 100.into()),
    ]);
    let mut store = snap_store_sqlite::Sqlite::open(&database).unwrap();
    store
        .run("persist credentials and session", |tx| {
            tx.insert(
                "identity.identities",
                [("id".into(), "owner".into())].into(),
            )?;
            tx.insert("identity.credentials", credential.clone())?;
            tx.insert("identity.sessions", session.clone())
        })
        .unwrap();
    drop(store);

    apply(&database, &migrations);
    let mut store = snap_store_sqlite::Sqlite::open(&database).unwrap();
    store.load("identity.credentials").unwrap();
    store.load("identity.sessions").unwrap();
    let (stored_credential, stored_session) = store
        .run("read persisted rows", |tx| {
            Ok((
                tx.get("identity.credentials", &["person@example.test".into()])?,
                tx.get(
                    "identity.sessions",
                    &[snap_store::Value::Bytes(vec![1; 32])],
                )?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(stored_credential, Some(credential));
    assert_eq!(stored_session, Some(session));
}
