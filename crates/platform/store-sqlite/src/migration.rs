use super::*;
use snap_store::migration::{Change, Migration};

#[derive(Debug)]
pub struct MigrationError(pub String);
impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for MigrationError {}
impl From<rusqlite::Error> for MigrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}
impl From<serde_json::Error> for MigrationError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}
#[derive(Debug)]
pub struct MigrationReport {
    pub applied: Vec<String>,
    pub pending: Vec<String>,
}

pub fn migrate(path: &Path, migrations: &[Migration]) -> Result<MigrationReport, MigrationError> {
    let mut connection = connect(path, true)?;
    apply(&mut connection, migrations)
}

pub fn status(path: &Path, migrations: &[Migration]) -> Result<MigrationReport, MigrationError> {
    let connection = connect(path, false)?;
    let (applied, catalog) = history(&connection)?;
    verify_shape(&connection)?;
    validate_history(&applied, migrations, catalog)?;
    Ok(MigrationReport {
        applied: applied.iter().map(|m| m.id.clone()).collect(),
        pending: migrations[applied.len()..]
            .iter()
            .map(|m| m.id.clone())
            .collect(),
    })
}

pub(crate) fn history(
    connection: &Connection,
) -> Result<(Vec<Migration>, Catalog), MigrationError> {
    let mut query =
        connection.prepare("SELECT id, definition FROM _snap_store_history ORDER BY id")?;
    let definitions = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut migrations = Vec::new();
    let mut catalog = Catalog::default();
    for (id, json) in definitions {
        let migration: Migration = serde_json::from_str(&json)?;
        if migration.id != id {
            return Err(MigrationError("migration history is inconsistent".into()));
        }
        catalog = migration
            .apply(&catalog)
            .map_err(|e| MigrationError(format!("invalid recorded migration {id}: {e:?}")))?;
        migrations.push(migration);
    }
    Ok((migrations, catalog))
}

fn validate_history(
    applied: &[Migration],
    migrations: &[Migration],
    mut catalog: Catalog,
) -> Result<(), MigrationError> {
    if migrations.len() < applied.len() || &migrations[..applied.len()] != applied {
        return Err(MigrationError(
            "applied migrations were changed, removed, or reordered; add a new migration instead"
                .into(),
        ));
    }
    if migrations.windows(2).any(|m| m[0].id >= m[1].id) {
        return Err(MigrationError(
            "migration IDs must be unique and strictly increasing".into(),
        ));
    }
    for migration in &migrations[applied.len()..] {
        catalog = migration
            .apply(&catalog)
            .map_err(|e| MigrationError(format!("invalid migration {}: {e:?}", migration.id)))?;
    }
    Ok(())
}

fn shape(connection: &Connection) -> Result<String, MigrationError> {
    let mut query = connection.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' AND name NOT IN ('_snap_store_history', '_snap_store_shape') ORDER BY type, name")?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::to_string(&rows)?)
}

pub(crate) fn verify_shape(connection: &Connection) -> Result<(), MigrationError> {
    // No adoption path for pre-program spike databases. Their rows have no
    // mutation history; recreate them explicitly rather than inventing a log.
    connection
        .prepare("SELECT position, program FROM _snap_store_programs LIMIT 0")
        .map_err(|_| {
            MigrationError("mutation program log is missing; recreate the spike database".into())
        })?;
    let recorded: String = connection.query_row(
        "SELECT definition FROM _snap_store_shape WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if recorded != shape(connection)? {
        return Err(MigrationError(
            "database DDL differs from recorded migrations".into(),
        ));
    }
    Ok(())
}

pub(crate) fn apply(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<MigrationReport, MigrationError> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;
    let initialized: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='_snap_store_history')",
        [],
        |r| r.get(0),
    )?;
    if !initialized {
        if shape(&tx)? != "[]" {
            return Err(MigrationError(
                "refusing to adopt an unmanaged database".into(),
            ));
        }
        tx.execute_batch("CREATE TABLE _snap_store_history (id TEXT PRIMARY KEY NOT NULL, definition TEXT NOT NULL) STRICT; CREATE TABLE _snap_store_shape (id INTEGER PRIMARY KEY CHECK(id=1), definition TEXT NOT NULL) STRICT; CREATE TABLE _snap_store_programs (position INTEGER PRIMARY KEY AUTOINCREMENT, program BLOB NOT NULL) STRICT;")?;
        tx.execute("INSERT INTO _snap_store_shape VALUES (1, ?)", [shape(&tx)?])?;
    }
    verify_shape(&tx)?;
    let (applied, mut catalog) = history(&tx)?;
    validate_history(&applied, migrations, catalog.clone())?;
    let mut added = Vec::new();
    for migration in &migrations[applied.len()..] {
        let next = migration
            .apply(&catalog)
            .map_err(|e| MigrationError(format!("invalid migration {}: {e:?}", migration.id)))?;
        for change in &migration.changes {
            ddl(&tx, change)?;
        }
        tx.execute(
            "INSERT INTO _snap_store_history VALUES (?, ?)",
            [&migration.id, &serde_json::to_string(migration)?],
        )?;
        catalog = next;
        added.push(migration.id.clone());
    }
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(MigrationError("migration violates foreign keys".into()));
    }
    tx.execute(
        "UPDATE _snap_store_shape SET definition=? WHERE id=1",
        [shape(&tx)?],
    )?;
    tx.commit()?;
    Ok(MigrationReport {
        applied: added,
        pending: Vec::new(),
    })
}

fn index_name(table: &str, index: &str) -> String {
    format!("{table}:{index}")
}
fn create_index(
    connection: &Connection,
    table: &str,
    index: &snap_store::Index,
) -> Result<(), MigrationError> {
    connection.execute_batch(&format!(
        "CREATE {} INDEX {} ON {} ({})",
        if index.unique { "UNIQUE" } else { "" },
        quote(&index_name(table, &index.name)),
        quote(table),
        columns(&index.columns)
    ))?;
    Ok(())
}

fn literal(value: &Value) -> Result<String, MigrationError> {
    Ok(match value {
        Value::Integer(v) => v.to_string(),
        Value::Text(v) => {
            if v.contains('\0') {
                return Err(MigrationError("a column fill cannot contain NUL".into()));
            }
            format!("'{}'", v.replace('\'', "''"))
        }
        Value::Bytes(v) => format!(
            "X'{}'",
            v.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ),
    })
}

fn ddl(connection: &Connection, change: &Change) -> Result<(), MigrationError> {
    match change {
        Change::CreateTable { table } => {
            let mut definitions: Vec<_> = table
                .columns
                .iter()
                .map(|c| format!("{} {} NOT NULL", quote(&c.name), sql_kind(c.kind)))
                .collect();
            definitions.push(format!("PRIMARY KEY ({})", columns(&table.primary)));
            for foreign in &table.foreign {
                definitions.push(format!(
                    "FOREIGN KEY ({}) REFERENCES {} ({}) DEFERRABLE INITIALLY DEFERRED",
                    columns(&foreign.columns),
                    quote(&foreign.table),
                    columns(&foreign.references)
                ));
            }
            connection.execute_batch(&format!(
                "CREATE TABLE {} ({}) STRICT, WITHOUT ROWID",
                quote(&table.name),
                definitions.join(",")
            ))?;
            for index in &table.indexes {
                create_index(connection, &table.name, index)?;
            }
        }
        Change::DropTable { table } => {
            connection.execute_batch(&format!("DROP TABLE {}", quote(table)))?;
        }
        Change::AddColumn {
            table,
            column,
            fill,
        } => {
            connection.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {} {} NOT NULL DEFAULT {}",
                quote(table),
                quote(&column.name),
                sql_kind(column.kind),
                literal(fill)?
            ))?;
        }
        Change::RenameColumn { table, from, to } => {
            connection.execute_batch(&format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                quote(table),
                quote(from),
                quote(to)
            ))?;
        }
        Change::CreateIndex { table, index } => create_index(connection, table, index)?,
        Change::DropIndex { table, index } => {
            connection
                .execute_batch(&format!("DROP INDEX {}", quote(&index_name(table, index))))?;
        }
    }
    Ok(())
}
