//! Shared Store cases take a fresh, cold Store with this schema. The backend
//! comes from the setup; expected rows and outcomes are independent literals.
use alloc::{collections::BTreeSet, vec, vec::Vec};
use snap_store::{
    Backend, Catalog, Column, Error, ForeignKey, Index, Kind, Row, RowChange, Store, Table,
    migration::{Change, Migration},
};

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        id: "0001_accounts".into(),
        changes: vec![
            Change::CreateTable {
                table: Table {
                    // Forward references in one migration are legal. The FK is checked
                    // at commit, so a caller can write the dependent row first.
                    name: "access.grants".into(),
                    columns: vec![Column {
                        name: "account".into(),
                        kind: Kind::Integer,
                    }],
                    primary: vec!["account".into()],
                    indexes: vec![],
                    foreign: vec![ForeignKey {
                        columns: vec!["account".into()],
                        table: "identity.accounts".into(),
                        references: vec!["id".into()],
                    }],
                },
            },
            Change::CreateTable {
                table: Table {
                    name: "identity.accounts".into(),
                    columns: vec![
                        Column {
                            name: "id".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "email".into(),
                            kind: Kind::Text,
                        },
                    ],
                    primary: vec!["id".into()],
                    indexes: vec![Index {
                        name: "email".into(),
                        columns: vec!["email".into()],
                        unique: true,
                    }],
                    foreign: vec![],
                },
            },
        ],
    }]
}

pub fn catalog() -> Catalog {
    migrations()[0].apply(&Catalog::default()).unwrap()
}

/// Programs execute the same transformations without invoking the authoring
/// handler. Reloaded reads independently exercise each backend's interpreter.
pub fn mutation_programs_replay_ordered_partial_updates_without_handlers<B: Backend>(
    store: &mut Store<B>,
) {
    for table in ["identity.accounts", "access.grants"] {
        store.load(table).unwrap();
    }
    let committed = store
        .run("author", |tx| {
            tx.insert("access.grants", Row::from([("account".into(), 1.into())]))?;
            tx.insert("identity.accounts", row(1, "alice"))?;
            tx.update(
                "identity.accounts",
                &[1.into()],
                Row::from([("email".into(), "bob".into())]),
            )?;
            assert_eq!(
                tx.get("identity.accounts", &[1.into()])?,
                Some(row(1, "bob"))
            );
            tx.update(
                "identity.accounts",
                &[1.into()],
                Row::from([("email".into(), "carol".into())]),
            )?;
            tx.insert("identity.accounts", row(2, "temporary"))?;
            tx.delete("identity.accounts", &[2.into()])?;
            Ok(())
        })
        .unwrap();
    let bytes = committed.program.as_bytes().to_vec();
    drop(committed);
    store
        .run("reset", |tx| {
            tx.delete("access.grants", &[1.into()])?;
            tx.delete("identity.accounts", &[1.into()])?;
            Ok(())
        })
        .unwrap();
    let program = snap_store::Program::from_bytes(store.catalog(), &bytes).unwrap();
    store.replay(&program).unwrap();
    for table in ["identity.accounts", "access.grants"] {
        store.load(table).unwrap();
    }
    assert_eq!(
        store
            .inspect("accounts", |tx| tx.find(
                "identity.accounts",
                "primary",
                &[]
            ))
            .unwrap(),
        vec![row(1, "carol")]
    );
    assert_eq!(
        store
            .inspect("grants", |tx| tx.find("access.grants", "primary", &[]))
            .unwrap(),
        vec![Row::from([("account".into(), 1.into())])]
    );
    assert_eq!(
        store
            .inspect("email", |tx| tx.find(
                "identity.accounts",
                "email",
                &["carol".into()]
            ))
            .unwrap(),
        vec![row(1, "carol")]
    );

    // A replay failure after staging an earlier instruction publishes nothing.
    let failed = store
        .run("author failure program", |tx| {
            tx.insert("identity.accounts", row(3, "dave"))?;
            tx.update(
                "identity.accounts",
                &[1.into()],
                Row::from([("email".into(), "eve".into())]),
            )
        })
        .unwrap()
        .program;
    store
        .run("remove replay target", |tx| {
            tx.delete("identity.accounts", &[3.into()])?;
            tx.delete("access.grants", &[1.into()])?;
            tx.delete("identity.accounts", &[1.into()])?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(store.replay(&failed), Err(Error::NotFound)));
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("failed replay", |tx| tx.find(
                "identity.accounts",
                "primary",
                &[]
            ))
            .unwrap()
            .is_empty()
    );
}

fn row(id: i64, email: &str) -> Row {
    Row::from([("id".into(), id.into()), ("email".into(), email.into())])
}

pub fn secondary_indexes_and_negative_results_change_with_the_commit<B: Backend>(
    store: &mut Store<B>,
) {
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("nx", |tx| tx.find(
                "identity.accounts",
                "email",
                &["alice".into()]
            ))
            .unwrap()
            .is_empty()
    );
    store
        .run("insert", |tx| {
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    store
        .run("update", |tx| {
            tx.update(
                "identity.accounts",
                &[1.into()],
                Row::from([("email".into(), "bob".into())]),
            )?;
            assert!(
                tx.find("identity.accounts", "email", &["alice".into()])?
                    .is_empty()
            );
            assert_eq!(
                tx.find("identity.accounts", "email", &["bob".into()])?,
                vec![row(1, "bob")]
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .inspect("bob", |tx| tx.find(
                "identity.accounts",
                "email",
                &["bob".into()]
            ))
            .unwrap(),
        vec![row(1, "bob")]
    );
    // Reload proves the backend applied Update, not just the resident projection.
    store.load("identity.accounts").unwrap();
    assert_eq!(
        store
            .inspect("reloaded", |tx| tx.find(
                "identity.accounts",
                "email",
                &["bob".into()]
            ))
            .unwrap(),
        vec![row(1, "bob")]
    );
    store
        .run("delete", |tx| tx.delete("identity.accounts", &[1.into()]))
        .unwrap();
    assert!(
        store
            .inspect("deleted", |tx| tx.find(
                "identity.accounts",
                "email",
                &["bob".into()]
            ))
            .unwrap()
            .is_empty()
    );
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("reloaded deletion", |tx| tx.find(
                "identity.accounts",
                "primary",
                &[]
            ))
            .unwrap()
            .is_empty()
    );
}

pub fn unique_index_failure_discards_earlier_statements_and_allows_the_next_operation<
    B: Backend,
>(
    store: &mut Store<B>,
) {
    store.load("identity.accounts").unwrap();
    store
        .run("alice", |tx| {
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    let failed = store.run("duplicate email", |tx| {
        tx.insert("identity.accounts", row(2, "bob"))?;
        tx.insert("identity.accounts", row(3, "alice"))
    });
    assert!(matches!(failed, Err(Error::Constraint)));
    assert_eq!(
        store
            .inspect("resident", |tx| tx.find(
                "identity.accounts",
                "primary",
                &[]
            ))
            .unwrap(),
        vec![row(1, "alice")]
    );
    store.load("identity.accounts").unwrap();
    assert_eq!(
        store
            .inspect("backend", |tx| tx.find("identity.accounts", "primary", &[]))
            .unwrap(),
        vec![row(1, "alice")]
    );
    store
        .run("bob", |tx| tx.insert("identity.accounts", row(2, "bob")))
        .unwrap();
}

pub fn cross_module_constraints_roll_back_every_write_including_memory<B: Backend>(
    store: &mut Store<B>,
) {
    for table in ["identity.accounts", "access.grants"] {
        store.load(table).unwrap();
    }
    let result = store.run("invalid signup", |tx| {
        tx.insert("identity.accounts", row(1, "alice"))?;
        tx.insert("access.grants", Row::from([("account".into(), 2.into())]))
    });
    assert!(matches!(result, Err(Error::Constraint)));
    assert!(
        store
            .inspect("resident", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .is_none()
    );
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("backend", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .is_none()
    );
    store
        .run("valid signup", |tx| {
            tx.insert("access.grants", Row::from([("account".into(), 1.into())]))?;
            tx.insert("identity.accounts", row(1, "alice"))
        })
        .unwrap();
    assert!(matches!(
        store.run("delete referenced", |tx| tx
            .delete("identity.accounts", &[1.into()])),
        Err(Error::Constraint)
    ));
    assert!(
        store
            .inspect("still present", |tx| tx
                .get("identity.accounts", &[1.into()]))
            .unwrap()
            .is_some()
    );
}

pub fn caught_miss_discards_writes_without_loading_or_retrying<B: Backend>(store: &mut Store<B>) {
    store.load("identity.accounts").unwrap();
    let mut attempts = 0;
    let result = store.run("signup", |tx| {
        attempts += 1;
        assert!(tx.get("identity.accounts", &[1.into()])?.is_none());
        tx.insert("identity.accounts", row(1, "alice"))?;
        // Catching a MISS must not unpoison the attempt or cause an implicit load.
        let _ = tx.get("access.grants", &[0.into()]);
        Ok(())
    });
    assert!(matches!(result, Err(Error::Miss(_))));
    assert_eq!(attempts, 1);
    assert_eq!(store.misses().count, 1);
    assert!(
        store
            .inspect("resident", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .is_none()
    );
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("backend", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.inspect("still cold", |tx| tx.get("access.grants", &[0.into()])),
        Err(Error::Miss(_))
    ));
    store.load("access.grants").unwrap();
    assert!(
        store
            .inspect("known nx", |tx| tx.get("access.grants", &[0.into()]))
            .unwrap()
            .is_none()
    );
}

pub fn cold_insert_does_not_claim_other_keys_or_a_complete_index<B: Backend>(store: &mut Store<B>) {
    store
        .run("insert", |tx| {
            tx.insert("identity.accounts", row(3, "alice"))?;
            assert_eq!(
                tx.get("identity.accounts", &[3.into()])?,
                Some(row(3, "alice"))
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .inspect("hot", |tx| tx.get("identity.accounts", &[3.into()]))
            .unwrap(),
        Some(row(3, "alice"))
    );
    assert!(matches!(
        store.inspect("cold", |tx| tx.get("identity.accounts", &[4.into()])),
        Err(Error::Miss(_))
    ));
    assert!(matches!(
        store.inspect("partial index", |tx| tx.find(
            "identity.accounts",
            "email",
            &["alice".into()]
        )),
        Err(Error::Miss(_))
    ));
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("nx", |tx| tx.get("identity.accounts", &[4.into()]))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .inspect("loaded", |tx| tx.get("identity.accounts", &[3.into()]))
            .unwrap(),
        Some(row(3, "alice"))
    );
}

pub fn committed_changes_coalesce_and_publish_only_the_net_state<B: Backend>(store: &mut Store<B>) {
    store.load("identity.accounts").unwrap();
    let row = row(1, "alice");
    let committed = store
        .run("create", |tx| {
            tx.insert("identity.accounts", row.clone())?;
            tx.delete("identity.accounts", &[1.into()])?;
            tx.insert("identity.accounts", row.clone())
        })
        .unwrap();
    assert_eq!(
        committed.changes,
        vec![RowChange {
            table: "identity.accounts".into(),
            key: vec![1.into()],
            before: None,
            after: Some(row.clone())
        }]
    );
    let committed = store
        .run("unchanged", |tx| {
            tx.delete("identity.accounts", &[1.into()])?;
            tx.insert("identity.accounts", row.clone())
        })
        .unwrap();
    assert!(committed.changes.is_empty());
    let committed = store
        .run("delete", |tx| tx.delete("identity.accounts", &[1.into()]))
        .unwrap();
    assert_eq!(
        committed.changes,
        vec![RowChange {
            table: "identity.accounts".into(),
            key: vec![1.into()],
            before: Some(row),
            after: None
        }]
    );
    store.load("identity.accounts").unwrap();
    assert!(
        store
            .inspect("backend", |tx| tx.find("identity.accounts", "primary", &[]))
            .unwrap()
            .is_empty()
    );
}

pub fn releasing_residency_does_not_delete_rows_or_claim_complete_indexes<B: Backend>(
    store: &mut Store<B>,
) {
    store
        .run("seed", |tx| {
            tx.insert("identity.accounts", row(1, "alice"))?;
            tx.insert("identity.accounts", row(2, "bob"))
        })
        .unwrap();
    store
        .retain_keys("identity.accounts", &BTreeSet::new())
        .unwrap();
    let keys = BTreeSet::from([vec![1.into()], vec![3.into()]]);
    store.load_keys("identity.accounts", &keys).unwrap();
    assert_eq!(
        store
            .inspect("one", |tx| tx.get("identity.accounts", &[1.into()]))
            .unwrap(),
        Some(row(1, "alice"))
    );
    assert!(
        store
            .inspect("absent", |tx| tx.get("identity.accounts", &[3.into()]))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.inspect("two", |tx| tx.get("identity.accounts", &[2.into()])),
        Err(Error::Miss(_))
    ));
    assert!(matches!(
        store.inspect("index", |tx| tx.find("identity.accounts", "primary", &[])),
        Err(Error::Miss(_))
    ));
    store
        .retain_keys("identity.accounts", &BTreeSet::new())
        .unwrap();
    assert!(matches!(
        store.inspect("released", |tx| tx.get("identity.accounts", &[1.into()])),
        Err(Error::Miss(_))
    ));
    store.load("identity.accounts").unwrap();
    assert_eq!(
        store
            .inspect("backend", |tx| tx.find("identity.accounts", "primary", &[]))
            .unwrap(),
        vec![row(1, "alice"), row(2, "bob")]
    );
}
