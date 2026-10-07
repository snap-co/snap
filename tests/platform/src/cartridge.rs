//! IO-free application behavior for Transport-to-Store conformance. This is not
//! the reference model: it uses real operation declarations and transactions.
pub mod benchmark;
use alloc::{string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_store::{
    Column, Data, Error as StoreError, Kind, Row, Table, Transaction,
    migration::{Change as Ddl, Migration},
};
use snap_transport::{
    Error, Operation, Value, json,
    operation::{Definition, Guard, TypedFailure},
};

pub const TABLES: [&str; 2] = ["probe.left", "probe.right"];
pub const COLD: &str = "probe.undeclared";

/// Two rows must move together. The deliberately undeclared table is used to
/// exercise a caught MISS after staging writes, not successful data loading.
pub fn migration() -> Migration {
    Migration {
        id: "9999_transport_probe".into(),
        changes: TABLES
            .into_iter()
            .chain([COLD])
            .map(|name| Ddl::CreateTable {
                table: Table {
                    name: name.into(),
                    columns: vec![
                        Column {
                            name: "id".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "value".into(),
                            kind: Kind::Integer,
                        },
                    ],
                    primary: vec!["id".into()],
                    indexes: vec![],
                    foreign: vec![],
                },
            })
            .collect(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stop {
    Commit,
    Application,
    InvalidOutput,
    CaughtMiss,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub expected: i64,
    pub amount: i64,
    pub stop: Stop,
}

/// A bounded example starting at zero. Both fixed host tests and the SDK journey
/// use these inputs; expected outcomes come from the independent dispatch model.
pub fn example() -> [Edit; 6] {
    [
        Edit {
            expected: 0,
            amount: 3,
            stop: Stop::Commit,
        },
        Edit {
            expected: 0,
            amount: 99,
            stop: Stop::Commit,
        },
        Edit {
            expected: 3,
            amount: 99,
            stop: Stop::Application,
        },
        Edit {
            expected: 3,
            amount: 99,
            stop: Stop::InvalidOutput,
        },
        Edit {
            expected: 3,
            amount: 99,
            stop: Stop::CaughtMiss,
        },
        Edit {
            expected: 3,
            amount: 2,
            stop: Stop::Commit,
        },
    ]
}

pub struct Change;
impl Operation for Change {
    const NAME: &'static str = "probe.change";
    type Input = Edit;
    type Output = [i64; 2];
    type Error = String;
    type Progress = Value;
}
pub struct Read;
impl Operation for Read {
    const NAME: &'static str = "probe.read";
    type Input = ();
    type Output = [i64; 2];
    type Error = String;
    type Progress = Value;
}

pub fn read(tx: &mut Transaction<'_>) -> Result<[i64; 2], StoreError> {
    let mut values = [0; 2];
    for (index, table) in TABLES.iter().enumerate() {
        let row = tx.get(table, &[1.into()])?.ok_or(StoreError::NotFound)?;
        let Some(snap_store::Value::Integer(value)) = row.get("value") else {
            return Err(StoreError::Invalid);
        };
        values[index] = *value;
    }
    Ok(values)
}

pub fn definitions() -> Vec<Definition> {
    let mut change = Definition::typed::<Change>(
        true,
        vec![Guard::new(|tx, invocation, _| {
            let edit: Edit = serde_json::from_value(invocation.input.clone())
                .map_err(|_| Error::InvalidInput)?;
            if read(tx)?[0] != edit.expected {
                return Err(Error::Application(json!("stale")).into());
            }
            Ok(())
        })],
        Data::new(&TABLES),
        &[],
        |tx, edit, _| {
            for (table, before) in TABLES.into_iter().zip(read(tx)?) {
                let after = before.checked_add(edit.amount).ok_or(StoreError::Invalid)?;
                tx.update(
                    table,
                    &[1.into()],
                    Row::from([("value".into(), after.into())]),
                )?;
            }
            match edit.stop {
                Stop::Commit => Ok(read(tx)?),
                Stop::Application => Err(TypedFailure::Application("declined".into())),
                Stop::InvalidOutput => Ok([i64::MAX; 2]),
                Stop::CaughtMiss => {
                    let output = read(tx)?;
                    let _ = tx.get(COLD, &[1.into()]);
                    // Return success despite the caught MISS. Only Store/runtime
                    // sticky-failure checks can stop these writes from committing.
                    Ok(output)
                }
            }
        },
    );
    // This cartridge's output contract excludes the sentinel. Returning it
    // deliberately exercises validation between handler execution and commit.
    change.output = |value| {
        serde_json::from_value::<[i64; 2]>(value.clone())
            .is_ok_and(|values| values.into_iter().all(|v| v != i64::MAX))
    };
    vec![
        change,
        Definition::typed::<Read>(true, vec![], Data::new(&TABLES), &[], |tx, (), _| {
            Ok(read(tx)?)
        }),
    ]
}
