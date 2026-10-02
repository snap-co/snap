//! The portable dispatch boundary owns acceptance, typed outcomes and rollback.
use snap_store::{Error as StoreError, Transaction};
use snap_transport::operation::{Context, Definition, Guard, Runtime, TypedFailure};
use snap_transport::{Error, Invocation, Operation, Value, json};

struct Increment;
impl Operation for Increment {
    const NAME: &'static str = "quota.increment";
    type Input = u64;
    type Output = u64;
    type Error = String;
    type Progress = Value;
}

fn counter(tx: &mut Transaction<'_>) -> Result<i64, StoreError> {
    let row = tx
        .get("quota.counter", &[1.into()])?
        .ok_or(StoreError::NotFound)?;
    match row.get("value") {
        Some(snap_store::Value::Integer(n)) => Ok(*n),
        _ => Err(StoreError::Invalid),
    }
}
fn write(tx: &mut Transaction<'_>, value: i64) -> Result<(), StoreError> {
    tx.update(
        "quota.counter",
        &[1.into()],
        [("value".into(), value.into())].into_iter().collect(),
    )
}
fn store() -> snap_store::Store<snap_store_sqlite::Sqlite> {
    let migrations = [toml::from_str(
        r#"
id = "0001_quota"
[[changes]]
action = "create_table"
[changes.table]
name = "quota.counter"
primary = ["id"]
columns = [{name="id",kind="integer"},{name="value",kind="integer"}]
"#,
    )
    .unwrap()];
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    store.load("quota.counter").unwrap();
    store
        .run("seed", |tx| {
            tx.insert(
                "quota.counter",
                [("id".into(), 1.into()), ("value".into(), 0.into())]
                    .into_iter()
                    .collect(),
            )
        })
        .unwrap();
    store
}

#[test]
fn accepted_typed_handler_composes_nested_writes_without_recharging_policy() {
    let mut store = store();
    let mut runtime = Runtime::default();
    let definition = Definition::typed::<Increment>(
        false,
        vec![Guard::new(|tx, _, context| {
            if counter(tx)? != 0 {
                return Err(Error::Application(json!("charged")).into());
            }
            context.prepared = json!("accepted");
            Ok(())
        })],
        snap_store::Data::new(&["quota.counter"]),
        &[],
        |tx, input, context| {
            assert_eq!(context.prepared, json!("accepted"));
            let value = counter(tx)?;
            write(tx, value + 1)?;
            // This is intentionally a second nested mutation. Rechecking admission
            // here would deny the accepted operation after its own first write.
            let value = counter(tx)?;
            write(tx, value + input as i64)?;
            Ok(counter(tx)? as u64)
        },
    );
    let selected = runtime.register(definition).unwrap();
    for id in 1..=2 {
        runtime
            .enqueue(
                id,
                Invocation {
                    id,
                    operation: Increment::NAME.into(),
                    input: json!(2),
                },
                selected,
            )
            .unwrap();
    }
    let (work, call, selection) = runtime.acquire().unwrap();
    runtime
        .accept(&mut store, work, call, selection, Context::default())
        .unwrap_or_else(|_| panic!("first admission"));
    assert!(runtime.acquire().is_none());
    let committed = runtime.execute(&mut store).unwrap();
    assert_eq!(committed.outcome, Ok(json!(3)));
    assert_eq!(committed.changes.len(), 1);
    assert!(
        runtime.acquire().is_none(),
        "publication still owns the gate"
    );
    runtime.finish();
    let (work, call, selection) = runtime.acquire().unwrap();
    assert!(
        matches!(runtime.accept(&mut store, work, call, selection, Context::default()), Err((2, Error::Application(v))) if v == json!("charged"))
    );
    runtime.reject();
    assert!(runtime.idle());
    assert_eq!(store.inspect("committed", counter).unwrap(), 3);
}

#[test]
fn declared_application_error_rolls_back_all_staged_writes() {
    let mut store = store();
    let mut runtime = Runtime::default();
    let selected = runtime
        .register(Definition::typed::<Increment>(
            false,
            vec![],
            snap_store::Data::new(&["quota.counter"]),
            &[],
            |tx, _, context| {
                write(tx, 99)?;
                context.publication = json!({"staged":99});
                Err(TypedFailure::Application("declined".into()))
            },
        ))
        .unwrap();
    assert_eq!(
        runtime.enqueue(
            (),
            Invocation {
                id: 1,
                operation: Increment::NAME.into(),
                input: json!("bad")
            },
            selected
        ),
        Err(Error::InvalidInput)
    );
    runtime
        .enqueue(
            (),
            Invocation {
                id: 2,
                operation: Increment::NAME.into(),
                input: json!(1),
            },
            selected,
        )
        .unwrap();
    let (work, call, selection) = runtime.acquire().unwrap();
    runtime
        .accept(&mut store, work, call, selection, Context::default())
        .unwrap_or_else(|_| panic!("zero guard admission"));
    let completed = runtime.execute(&mut store).unwrap();
    assert_eq!(
        completed.outcome,
        Err(Error::Application(json!("declined")))
    );
    assert!(completed.changes.is_empty());
    assert!(completed.storage_failure.is_none());
    assert!(completed.context.publication.is_null());
    runtime.finish();
    assert_eq!(store.inspect("rollback", counter).unwrap(), 0);
}

#[test]
fn bearer_callbacks_are_discarded_on_handler_validation_and_commit_failure() {
    use snap_store::{Backend, Catalog, CommitError, Row, Store, Table, Write};
    use snap_transport::bearer::{Change, Receiver, Token};
    struct Disk(u8);
    impl Backend for Disk {
        fn load(&mut self, _: &Table) -> Result<Vec<Row>, StoreError> {
            Ok(vec![])
        }
        fn commit(&mut self, _: &[Write]) -> Result<(), CommitError> {
            match self.0 {
                1 => Err(CommitError::Rejected(StoreError::Unavailable)),
                2 => Err(CommitError::Indeterminate),
                _ => Ok(()),
            }
        }
    }
    for (fault, failure) in [(0, 0), (0, 1), (0, 2), (1, 0), (2, 0)] {
        let migration: snap_store::migration::Migration = toml::from_str(
            r#"id="0001_tokens"
[[changes]]
action="create_table"
[changes.table]
name="private.tokens"
primary=["id"]
columns=[{name="id",kind="integer"}]
"#,
        )
        .unwrap();
        let mut store =
            Store::new(migration.apply(&Catalog::default()).unwrap(), Disk(fault)).unwrap();
        store.load("private.tokens").unwrap();
        let mut runtime = Runtime::default();
        let mut definition = Definition::typed::<Increment>(
            false,
            vec![],
            snap_store::Data::default(),
            &[],
            move |tx, _, context| {
                tx.insert(
                    "private.tokens",
                    [("id".into(), 1.into())].into_iter().collect(),
                )?;
                context
                    .bearer_changed(Change::Set(Token::new("secret".into())))
                    .unwrap();
                if failure == 1 {
                    return Err(TypedFailure::Application("abort".into()));
                }
                Ok(1)
            },
        );
        if failure == 2 {
            definition.output = |_| false;
        }
        let selection = runtime.register(definition).unwrap();
        runtime
            .enqueue(
                (),
                Invocation {
                    id: 1,
                    operation: Increment::NAME.into(),
                    input: json!(1),
                },
                selection,
            )
            .unwrap();
        let (work, call, selection) = runtime.acquire().unwrap();
        runtime
            .accept(&mut store, work, call, selection, Context::default())
            .unwrap_or_else(|_| panic!("admission"));
        let completed = runtime.execute(&mut store).unwrap();
        if fault == 0 && failure == 0 {
            assert_eq!(completed.outcome, Ok(json!(1)));
            assert!(matches!(
                completed.context.bearer_change,
                Some(Change::Set(_))
            ));
        } else {
            assert!(completed.outcome.is_err());
            assert!(completed.context.bearer_change.is_none());
            assert!(completed.changes.is_empty());
        }
    }
}
