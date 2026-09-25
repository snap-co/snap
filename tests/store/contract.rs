#[path = "../adapters/store.rs"]
mod adapter;
mod shared;

#[tokio::test]
async fn memory_store_contract() {
    let fixture = adapter::Fixture::new(false, &shared::schemas());
    shared::contract(
        fixture.store.clone(),
        fixture.peer.clone(),
        snap_native::store::MemoryCache::new(8),
    )
    .await;
}

#[tokio::test]
async fn sqlite_store_contract() {
    let fixture = adapter::Fixture::new(true, &shared::schemas());
    shared::contract(
        fixture.store.clone(),
        fixture.peer.clone(),
        snap_native::store::MemoryCache::new(8),
    )
    .await;
}

#[tokio::test]
async fn identifiers_cannot_alias_namespace_ownership() {
    use snap_store::{Error, Query, Statement, Store, Table, Transaction};
    for sqlite in [false, true] {
        let schemas = shared::schemas();
        let fixture = adapter::Fixture::new(sqlite, &schemas);
        let row = [
            ("id".into(), "r".into()),
            ("locator".into(), "original".into()),
            ("revision".into(), 1_i64.into()),
        ]
        .into();
        fixture
            .store
            .transaction(Transaction {
                guards: vec![],
                statements: vec![Statement::Insert {
                    table: schemas[0].table,
                    row,
                }],
            })
            .await
            .unwrap();
        let mut mixed = schemas[0].clone();
        mixed.table.namespace = "Documents";
        assert!(matches!(
            fixture.register(&[schemas[0].clone(), mixed.clone()]),
            Err(Error::Invalid)
        ));
        assert!(matches!(fixture.register(&[mixed]), Err(Error::Invalid)));
        for table in [
            Table {
                namespace: "snap",
                name: "STORE_SCHEMAS",
            },
            Table {
                namespace: "documents",
                name: "Records",
            },
        ] {
            let mut invalid = schemas[0].clone();
            invalid.table = table;
            assert!(matches!(fixture.register(&[invalid]), Err(Error::Invalid)));
        }
        let mut legacy = schemas[0].clone();
        legacy.legacy_name = Some("DOCUMENTS_RECORDS");
        assert!(matches!(fixture.register(&[legacy]), Err(Error::Invalid)));
        let read = Transaction {
            guards: vec![],
            statements: vec![Statement::Select(Query::new(schemas[0].table))],
        };
        let rows = fixture.store.transaction(read.clone()).await.unwrap();
        assert_eq!(rows[0][0]["locator"], "original".into());
        if sqlite {
            assert_eq!(
                fixture
                    .register(&schemas)
                    .unwrap()
                    .transaction(read)
                    .await
                    .unwrap(),
                rows
            );
        }
    }
}
