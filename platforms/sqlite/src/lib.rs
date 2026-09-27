//! SQLite's transaction is the durability authority in phase one. This adapter
//! holds an exclusive SQLite lock for its entire lifetime, including idle periods.
//! It intentionally prevents concurrent Store owners and migrations. No server
//! read can bypass the portable resident transaction interface.
mod migration;
pub use migration::{MigrationError, MigrationReport, migrate, status};
use rusqlite::{Connection, params_from_iter};
use snap_store::resident::{Backend, Catalog, CommitError, Error, Store, Table, Write};
use snap_store::{Kind, Row, Rows, Value};
use std::path::Path;

pub struct Sqlite {
    connection: Connection,
    catalog: Catalog,
}

impl Sqlite {
    /// Open an explicitly migrated database. No DDL or automatic migration here.
    pub fn open(path: &Path) -> Result<Store<Self>, MigrationError> {
        let connection = connect(path, false)?;
        let (_, catalog) = migration::history(&connection)?;
        migration::verify_shape(&connection)?;
        Store::new(
            catalog.clone(),
            Self {
                connection,
                catalog,
            },
        )
        .map_err(|e| MigrationError(format!("invalid catalog: {e:?}")))
    }

    /// Ephemeral SQLite for tests/experiments. Same SQL/transaction semantics,
    /// explicitly no persistence across process termination.
    pub fn memory(
        migrations: &[snap_store::resident::migration::Migration],
    ) -> Result<Store<Self>, MigrationError> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        migration::apply(&mut connection, migrations)?;
        let (_, catalog) = migration::history(&connection)?;
        Store::new(
            catalog.clone(),
            Self {
                connection,
                catalog,
            },
        )
        .map_err(|e| MigrationError(format!("invalid catalog: {e:?}")))
    }
}

fn configure(connection: &Connection) -> Result<(), MigrationError> {
    // A missing quoted identifier must be an error, never a string constant in
    // an index expression. SQLite otherwise accepts some invalid DDL silently.
    connection.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
    connection.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
    connection.busy_timeout(std::time::Duration::ZERO)?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA synchronous=EXTRA; PRAGMA locking_mode=EXCLUSIVE;",
    )?;
    Ok(())
}

fn connect(path: &Path, create: bool) -> Result<Connection, MigrationError> {
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Err(MigrationError("use a filesystem database path".into()));
    }
    if create {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| MigrationError(e.to_string()))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .map_err(|e| MigrationError(e.to_string()))?;
    }
    let connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(&connection)?;
    connection.execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE; COMMIT;")?;
    Ok(connection)
}

pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
pub(crate) fn columns(names: &[String]) -> String {
    names
        .iter()
        .map(|n| quote(n))
        .collect::<Vec<_>>()
        .join(", ")
}
pub(crate) fn sql_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "TEXT",
        Kind::Integer => "INTEGER",
        Kind::Bytes => "BLOB",
    }
}
fn sql_value(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(v) => rusqlite::types::Value::Text(v.clone()),
        Value::Integer(v) => rusqlite::types::Value::Integer(*v),
        Value::Bytes(v) => rusqlite::types::Value::Blob(v.clone()),
    }
}
fn sql_error(error: rusqlite::Error) -> Error {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::ConstraintViolation) => Error::Constraint,
        _ => Error::Unavailable,
    }
}

impl Backend for Sqlite {
    fn load(&mut self, table: &Table) -> Result<Rows, Error> {
        let names: Vec<_> = table.columns.iter().map(|c| c.name.clone()).collect();
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {} FROM {} ORDER BY {}",
                columns(&names),
                quote(&table.name),
                columns(&table.primary)
            ))
            .map_err(sql_error)?;
        statement
            .query_map([], |r| {
                let mut row = Row::new();
                for (i, column) in table.columns.iter().enumerate() {
                    let value = match column.kind {
                        Kind::Text => Value::Text(r.get(i)?),
                        Kind::Integer => Value::Integer(r.get(i)?),
                        Kind::Bytes => Value::Bytes(r.get(i)?),
                    };
                    row.insert(column.name.clone(), value);
                }
                Ok(row)
            })
            .map_err(sql_error)?
            .collect::<Result<_, _>>()
            .map_err(sql_error)
    }

    fn commit(&mut self, writes: &[Write]) -> Result<(), CommitError> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)
            .map_err(|e| CommitError::Rejected(sql_error(e)))?;
        for write in writes {
            let result = (|| {
                let (name, key) = match write {
                    Write::Insert { table, .. } => (table, None),
                    Write::Update { table, key, .. } | Write::Delete { table, key } => {
                        (table, Some(key))
                    }
                };
                let schema = self.catalog.table(name)?;
                let names: Vec<_> = schema.columns.iter().map(|c| c.name.clone()).collect();
                let condition = schema
                    .primary
                    .iter()
                    .map(|c| format!("{} = ?", quote(c)))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let mut values = Vec::new();
                let sql = match write {
                    Write::Insert { row, .. } => {
                        schema.validate_row(row)?;
                        values.extend(names.iter().map(|c| sql_value(&row[c])));
                        format!(
                            "INSERT INTO {} ({}) VALUES ({})",
                            quote(name),
                            columns(&names),
                            vec!["?"; names.len()].join(",")
                        )
                    }
                    Write::Update { row, .. } => {
                        schema.validate_row(row)?;
                        values.extend(names.iter().map(|c| sql_value(&row[c])));
                        format!(
                            "UPDATE {} SET {} WHERE {condition}",
                            quote(name),
                            names
                                .iter()
                                .map(|c| format!("{} = ?", quote(c)))
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                    }
                    Write::Delete { .. } => {
                        format!("DELETE FROM {} WHERE {condition}", quote(name))
                    }
                };
                if let Some(key) = key {
                    values.extend(key.iter().map(sql_value));
                }
                if tx
                    .execute(&sql, params_from_iter(values))
                    .map_err(sql_error)?
                    != 1
                {
                    return Err(Error::Constraint);
                }
                Ok(())
            })();
            if let Err(error) = result {
                return match tx.rollback() {
                    Ok(()) => Err(CommitError::Rejected(error)),
                    Err(_) => Err(CommitError::Indeterminate),
                };
            }
        }
        // Deferred FK constraints fail at COMMIT. Explicit SQL keeps the tx alive
        // so rollback can be checked, rather than silently ignored by Drop.
        match tx.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(error) => {
                let constraint =
                    error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation);
                let rolled_back = tx.rollback().is_ok();
                if constraint && rolled_back {
                    Err(CommitError::Rejected(Error::Constraint))
                } else {
                    Err(CommitError::Indeterminate)
                }
            }
        }
    }
}
