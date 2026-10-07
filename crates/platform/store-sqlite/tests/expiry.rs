use snap_store::{
    Column, Error, Index, Kind, Row, Table, Value,
    migration::{Change, Migration},
};
use snap_store_sqlite::Sqlite;

#[test]
fn cold_expiry_batches_preserve_live_rows_without_establishing_full_residency() {
    let table = Table {
        name: "fixture.expiring".into(),
        primary: vec!["id".into()],
        columns: vec![
            Column {
                name: "id".into(),
                kind: Kind::Integer,
            },
            Column {
                name: "expires".into(),
                kind: Kind::Integer,
            },
        ],
        indexes: vec![Index {
            name: "expires".into(),
            columns: vec!["expires".into()],
            unique: false,
        }],
        foreign: vec![],
    };
    let mut store = Sqlite::memory(&[Migration {
        id: "0001_expiry".into(),
        changes: vec![Change::CreateTable { table }],
    }])
    .unwrap();
    store
        .run("seed", |tx| {
            for (id, expires) in [(1, 100), (2, 9), (3, 10), (4, 11)] {
                tx.insert(
                    "fixture.expiring",
                    Row::from([
                        ("id".into(), Value::Integer(id)),
                        ("expires".into(), Value::Integer(expires)),
                    ]),
                )?;
            }
            Ok(())
        })
        .unwrap();
    store
        .retain_keys("fixture.expiring", &Default::default())
        .unwrap();
    for id in [2i64, 3] {
        let result = store
            .prune_expired("fixture.expiring", "expires", 10, 1)
            .unwrap();
        assert_eq!(result.value, 1);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].key, vec![id.into()]);
        assert!(result.changes[0].after.is_none());
        assert!(matches!(
            store.inspect("still cold", |tx| tx.find(
                "fixture.expiring",
                "expires",
                &[]
            )),
            Err(Error::Miss(_))
        ));
    }
    assert_eq!(
        store
            .prune_expired("fixture.expiring", "expires", 10, 1)
            .unwrap()
            .value,
        0
    );
    store.load("fixture.expiring").unwrap();
    let rows = store
        .inspect("live rows", |tx| {
            tx.find("fixture.expiring", "primary", &[])
        })
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
        vec![1i64.into(), 4i64.into()]
    );
    let rows = store
        .inspect("inclusive bounded resident scan", |tx| {
            tx.find_through("fixture.expiring", "expires", 100, 1)
        })
        .unwrap();
    assert_eq!(rows[0]["id"], Value::Integer(4));
    assert_eq!(
        store
            .inspect("capped count", |tx| tx.count_up_to(
                "fixture.expiring",
                "primary",
                &[],
                1
            ))
            .unwrap(),
        1
    );
}
