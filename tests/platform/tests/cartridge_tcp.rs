//! Paired composition proof missing from the independent Store/carrier suites:
//! real client SDK -> TCP/TLS -> native host -> file SQLite -> client SDK.
#[path = "../support/tcp_sqlite.rs"]
mod setup;

#[tokio::test]
async fn cartridge_crosses_tcp_and_survives_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cartridge.sqlite");
    let rows = setup::run(&database, |_| {}).await.unwrap();
    assert_eq!(rows, [5, 5]);
}
