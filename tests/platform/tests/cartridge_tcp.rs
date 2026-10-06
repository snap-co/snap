//! Paired composition proof missing from the independent Store/carrier suites:
//! real client SDK -> TCP/TLS -> native host -> file SQLite -> client SDK.
#[path = "../support/host.rs"]
mod host;
#[path = "../support/tcp_sqlite.rs"]
mod setup;

#[tokio::test]
async fn cartridge_crosses_tcp_and_survives_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cartridge.sqlite");
    let rows = setup::run(&database, |_| {}).await.unwrap();
    assert_eq!(rows, [5, 5]);
}

#[tokio::test]
async fn existing_database_and_companion_files_are_preserved() {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("cartridge.sqlite");
        let existing = directory.path().join(format!("cartridge.sqlite{suffix}"));
        std::fs::write(&existing, b"existing file must remain unchanged").unwrap();
        let error = setup::run(&database, |_| {}).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"existing file must remain unchanged"
        );
        if !suffix.is_empty() {
            assert!(
                !database.exists(),
                "collision must be rejected before creating the database"
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_companion_symlinks_are_preserved_without_creating_targets() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("cartridge.sqlite");
        let existing = directory.path().join(format!("cartridge.sqlite{suffix}"));
        let target = directory.path().join("absent-target");
        std::os::unix::fs::symlink(&target, &existing).unwrap();
        let error = setup::run(&database, |_| {}).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_link(&existing).unwrap(), target);
        assert!(!target.exists());
        assert!(!database.exists());
    }
}
