use snap_sqlite::{Sqlite, migrate};
use snap_store::resident::{migration::*, *};
use snap_store::{Kind, Row};

fn migration() -> Migration {
    Migration {
        id: "0001_ledger".into(),
        changes: vec![Change::CreateTable {
            table: Table {
                name: "ledger.entries".into(),
                columns: vec![Column {
                    name: "id".into(),
                    kind: Kind::Integer,
                }],
                primary: vec!["id".into()],
                indexes: vec![],
                foreign: vec![],
            },
        }],
    }
}

#[test]
fn crash_child() {
    let Ok(path) = std::env::var("SNAP_STORE_CRASH_DB") else {
        return;
    };
    let phase = std::env::var("SNAP_STORE_CRASH_PHASE").unwrap();
    let mut store = Sqlite::open(std::path::Path::new(&path)).unwrap();
    store
        .run("write", |tx| {
            tx.insert("ledger.entries", Row::from([("id".into(), 1.into())]))?;
            if phase == "before" {
                std::process::exit(0);
            }
            Ok(())
        })
        .unwrap();
    // Exit without dropping Store/Connection, after successful acknowledgement.
    std::process::exit(0);
}

#[test]
#[ignore = "cross-process recovery gate; run explicitly with --ignored"]
fn abrupt_process_exit_preserves_commits_and_discards_uncommitted_attempts() {
    for phase in ["before", "after"] {
        let path = std::env::temp_dir().join(format!(
            "snap-store-crash-{}-{phase}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        migrate(&path, &[migration()]).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_child", "--nocapture"])
            .env("SNAP_STORE_CRASH_DB", &path)
            .env("SNAP_STORE_CRASH_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut store = Sqlite::open(&path).unwrap();
        store.load("ledger.entries").unwrap();
        let row = store
            .run("read", |tx| tx.get("ledger.entries", &[1.into()]))
            .unwrap()
            .value;
        assert_eq!(row.is_some(), phase == "after");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
