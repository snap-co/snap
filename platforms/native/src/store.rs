//! Host-owned shared Store. SQL is confined here; providers declare logical schemas.
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use snap_store::{
    Compare, Error, Kind, Predicate, Query, Row, Rows, Schema, Statement, Table, Transaction, Value,
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

enum Backend {
    Memory(BTreeMap<Table, Rows>),
    Sqlite(Connection),
}
struct State {
    backend: Backend,
    schemas: BTreeMap<Table, Schema>,
}
#[derive(Clone)]
pub struct Store(Arc<Mutex<State>>);

impl Store {
    pub fn memory(schemas: &[Schema]) -> Result<Self, Error> {
        validate_schemas(schemas)?;
        Ok(Self(Arc::new(Mutex::new(State {
            backend: Backend::Memory(schemas.iter().map(|s| (s.table, Vec::new())).collect()),
            schemas: schemas.iter().cloned().map(|s| (s.table, s)).collect(),
        }))))
    }

    /// Startup registration is transactional, including legacy-name migration.
    /// Existing schema fingerprints must match; incompatible upgrades fail closed
    /// until the owning module supplies an explicit migration in a later version.
    pub fn sqlite(path: &Path, schemas: &[Schema]) -> Result<Self, Error> {
        validate_schemas(schemas)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|_| Error::Unavailable)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(|_| Error::Unavailable)?;
        }
        let mut db = Connection::open(path).map_err(sql_error)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_error)?;
        db.execute_batch("PRAGMA foreign_keys=ON")
            .map_err(sql_error)?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS snap_store_schemas (physical TEXT PRIMARY KEY, logical TEXT NOT NULL, shape TEXT NOT NULL)").map_err(sql_error)?;
        for schema in schemas {
            let physical = name(schema.table);
            let logical = format!("{}.{}", schema.table.namespace, schema.table.name);
            let shape = format!(
                "{:?}/{:?}/{:?}/{:?}",
                schema.columns, schema.primary, schema.indexes, schema.foreign
            );
            let registered: Option<(String, String)> = tx
                .query_row(
                    "SELECT logical, shape FROM snap_store_schemas WHERE physical=?1 COLLATE NOCASE",
                    [&physical],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            if registered
                .as_ref()
                .is_some_and(|r| r != &(logical.clone(), shape.clone()))
            {
                return Err(Error::Invalid);
            }
            let exists = |table: &str| -> Result<bool, Error> {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |r| r.get(0),
                )
                .map_err(sql_error)
            };
            if let Some(legacy) = schema.legacy_name
                && exists(legacy)?
            {
                if exists(&physical)? {
                    return Err(Error::Invalid);
                }
                tx.execute_batch(&format!(
                    "ALTER TABLE {} RENAME TO {}",
                    quote(legacy),
                    quote(&physical)
                ))
                .map_err(sql_error)?;
            }
            let fields = schema
                .columns
                .iter()
                .map(|(column, kind)| {
                    format!(
                        "{} {} NOT NULL",
                        quote(column),
                        match kind {
                            Kind::Text => "TEXT",
                            Kind::Integer => "INTEGER",
                            Kind::Bytes => "BLOB",
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let foreign = schema
                .foreign
                .iter()
                .map(|f| {
                    format!(
                        ", FOREIGN KEY ({}) REFERENCES {} ({})",
                        columns(f.columns),
                        quote(&name(f.target)),
                        columns(f.references)
                    )
                })
                .collect::<String>();
            tx.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {} ({fields}, PRIMARY KEY ({}){foreign})",
                quote(&physical),
                columns(schema.primary)
            ))
            .map_err(sql_error)?;
            // Verify legacy/unregistered tables before declaring them migrated.
            let actual = tx
                .prepare(&format!("PRAGMA table_info({})", quote(&physical)))
                .map_err(sql_error)?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u32>(3)?,
                        r.get::<_, u32>(5)? as usize,
                    ))
                })
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            if actual.len() != schema.columns.len()
                || schema.columns.iter().any(|(column, kind)| {
                    !actual.iter().any(|(c, k, not_null, primary)| {
                        c == column
                            && (*not_null == 1 || schema.primary.contains(column))
                            && *primary
                                == schema
                                    .primary
                                    .iter()
                                    .position(|p| p == column)
                                    .map_or(0, |i| i + 1)
                            && k.eq_ignore_ascii_case(match kind {
                                Kind::Text => "TEXT",
                                Kind::Integer => "INTEGER",
                                Kind::Bytes => "BLOB",
                            })
                    })
                })
            {
                return Err(Error::Invalid);
            }
            for (i, index) in schema.indexes.iter().enumerate() {
                tx.execute_batch(&format!(
                    "CREATE {} INDEX IF NOT EXISTS {} ON {} ({})",
                    if index.unique { "UNIQUE" } else { "" },
                    quote(&format!("{physical}_i{i}")),
                    quote(&physical),
                    columns(index.columns)
                ))
                .map_err(sql_error)?;
                let index_name = format!("{physical}_i{i}");
                let indexed = tx
                    .prepare(&format!("PRAGMA index_info({})", quote(&index_name)))
                    .map_err(sql_error)?
                    .query_map([], |r| r.get::<_, String>(2))
                    .map_err(sql_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sql_error)?;
                let indexes = tx
                    .prepare(&format!("PRAGMA index_list({})", quote(&physical)))
                    .map_err(sql_error)?
                    .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))
                    .map_err(sql_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sql_error)?;
                if indexed != index.columns || !indexes.contains(&(index_name, index.unique)) {
                    return Err(Error::Invalid);
                }
            }
            tx.execute(
                "INSERT OR IGNORE INTO snap_store_schemas VALUES (?1,?2,?3)",
                [&physical, &logical, &shape],
            )
            .map_err(sql_error)?;
        }
        for schema in schemas {
            let mut actual = tx
                .prepare(&format!(
                    "PRAGMA foreign_key_list({})",
                    quote(&name(schema.table))
                ))
                .map_err(sql_error)?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                })
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            let mut expected = schema
                .foreign
                .iter()
                .flat_map(|f| {
                    f.columns.iter().zip(f.references).map(move |(c, r)| {
                        (
                            name(f.target),
                            (*c).into(),
                            (*r).into(),
                            "NO ACTION".into(),
                            "NO ACTION".into(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            actual.sort();
            expected.sort();
            if actual != expected {
                return Err(Error::Invalid);
            }
        }
        if tx
            .prepare("PRAGMA foreign_key_check")
            .map_err(sql_error)?
            .exists([])
            .map_err(sql_error)?
        {
            return Err(Error::Constraint);
        }
        tx.commit().map_err(sql_error)?;
        Ok(Self(Arc::new(Mutex::new(State {
            backend: Backend::Sqlite(db),
            schemas: schemas.iter().cloned().map(|s| (s.table, s)).collect(),
        }))))
    }

    /// Synchronous entry point for startup and the host's blocking worker only.
    pub fn execute(&self, transaction: Transaction) -> Result<Vec<Rows>, Error> {
        let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
        validate_transaction(&state.schemas, &transaction)?;
        let State { backend, schemas } = &mut *state;
        match backend {
            Backend::Memory(data) => {
                let mut next = data.clone();
                for guard in transaction.guards {
                    if !select_memory(&next, schemas, &guard.query).is_empty() != guard.exists {
                        return Err(Error::Conflict);
                    }
                }
                let mut results = Vec::new();
                for statement in transaction.statements {
                    match statement {
                        Statement::Select(query) => {
                            results.push(select_memory(&next, schemas, &query))
                        }
                        Statement::Delete { table, filter } => {
                            next.get_mut(&table)
                                .unwrap()
                                .retain(|row| !matches(row, &filter));
                            validate_data(&next, schemas)?;
                            results.push(Vec::new());
                        }
                        Statement::Update {
                            table,
                            filter,
                            changes,
                        } => {
                            for row in next
                                .get_mut(&table)
                                .unwrap()
                                .iter_mut()
                                .filter(|r| matches(r, &filter))
                            {
                                row.extend(changes.clone());
                            }
                            validate_data(&next, schemas)?;
                            results.push(Vec::new());
                        }
                        Statement::Insert { table, row } => {
                            let schema = &schemas[&table];
                            let rows = next.get_mut(&table).unwrap();
                            for keys in core::iter::once(schema.primary).chain(
                                schema
                                    .indexes
                                    .iter()
                                    .filter(|i| i.unique)
                                    .map(|i| i.columns),
                            ) {
                                if rows.iter().any(|other| {
                                    keys.iter().all(|key| other.get(*key) == row.get(*key))
                                }) {
                                    return Err(Error::Constraint);
                                }
                            }
                            rows.push(row);
                            validate_data(&next, schemas)?;
                            results.push(Vec::new());
                        }
                    }
                }
                *data = next;
                Ok(results)
            }
            Backend::Sqlite(db) => {
                let tx = db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(sql_error)?;
                for guard in transaction.guards {
                    if !select_sql(&tx, schemas, &guard.query)?.is_empty() != guard.exists {
                        return Err(Error::Conflict);
                    }
                }
                let mut results = Vec::new();
                for statement in transaction.statements {
                    match statement {
                        Statement::Select(query) => results.push(select_sql(&tx, schemas, &query)?),
                        Statement::Delete { table, filter } => {
                            tx.execute(
                                &format!(
                                    "DELETE FROM {}{}",
                                    quote(&name(table)),
                                    where_sql(&filter)
                                ),
                                params_from_iter(filter.iter().map(|p| sql_value(&p.value))),
                            )
                            .map_err(sql_error)?;
                            results.push(Vec::new());
                        }
                        Statement::Update {
                            table,
                            filter,
                            changes,
                        } => {
                            let sets = changes
                                .keys()
                                .map(|c| format!("{}=?", quote(c)))
                                .collect::<Vec<_>>()
                                .join(",");
                            tx.execute(
                                &format!(
                                    "UPDATE {} SET {sets}{}",
                                    quote(&name(table)),
                                    where_sql(&filter)
                                ),
                                params_from_iter(
                                    changes
                                        .values()
                                        .map(sql_value)
                                        .chain(filter.iter().map(|p| sql_value(&p.value))),
                                ),
                            )
                            .map_err(sql_error)?;
                            results.push(Vec::new());
                        }
                        Statement::Insert { table, row } => {
                            tx.execute(
                                &format!(
                                    "INSERT INTO {} ({}) VALUES ({})",
                                    quote(&name(table)),
                                    row.keys().map(|k| quote(k)).collect::<Vec<_>>().join(","),
                                    vec!["?"; row.len()].join(",")
                                ),
                                params_from_iter(row.values().map(sql_value)),
                            )
                            .map_err(sql_error)?;
                            results.push(Vec::new());
                        }
                    }
                }
                tx.commit().map_err(sql_error)?;
                Ok(results)
            }
        }
    }
}
impl snap_store::Store for Store {
    async fn transaction(&self, transaction: Transaction) -> Result<Vec<Rows>, Error> {
        let store = self.clone();
        // Dropping a waiter cannot cancel an already-started blocking transaction.
        tokio::task::spawn_blocking(move || store.execute(transaction))
            .await
            .map_err(|_| Error::Unavailable)?
    }
}

#[derive(Clone)]
pub struct MemoryCache {
    entries: Arc<Mutex<BTreeMap<Query, Rows>>>,
    capacity: usize,
}
impl MemoryCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Arc::default(),
            capacity,
        }
    }
}
impl snap_store::Cache for MemoryCache {
    fn get(&self, query: &Query) -> Option<Rows> {
        self.entries.lock().ok()?.get(query).cloned()
    }
    fn put(&self, query: Query, rows: Rows) {
        if self.capacity == 0 {
            return;
        }
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= self.capacity {
                entries.pop_first();
            }
            entries.insert(query, rows);
        }
    }
}

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn name(table: Table) -> String {
    format!("{}_{}", table.namespace, table.name)
}
fn quote(value: &str) -> String {
    format!("\"{value}\"")
}
fn columns(names: &[&str]) -> String {
    names.iter().map(|c| quote(c)).collect::<Vec<_>>().join(",")
}
fn validate_schemas(schemas: &[Schema]) -> Result<(), Error> {
    let mut names = Vec::new();
    for schema in schemas {
        let physical = name(schema.table);
        if physical == "snap_store_schemas"
            || names.contains(&physical)
            || !valid(schema.table.namespace)
            || !valid(schema.table.name)
            || schema.columns.is_empty()
            || schema.primary.is_empty()
            || schema
                .legacy_name
                .is_some_and(|n| !valid(n) || n == physical || n == "snap_store_schemas")
        {
            return Err(Error::Invalid);
        }
        names.push(physical);
        let mut cols = Vec::new();
        for (column, _) in schema.columns {
            if !valid(column) || cols.contains(column) {
                return Err(Error::Invalid);
            }
            cols.push(*column);
        }
        if core::iter::once(schema.primary)
            .chain(schema.indexes.iter().map(|i| i.columns))
            .any(|keys| keys.is_empty() || keys.iter().any(|key| !cols.contains(key)))
        {
            return Err(Error::Invalid);
        }
        for foreign in schema.foreign {
            let target = schemas
                .iter()
                .find(|s| s.table == foreign.target)
                .ok_or(Error::Invalid)?;
            if foreign.columns.is_empty()
                || foreign.columns.len() != foreign.references.len()
                || foreign.references != target.primary
            {
                return Err(Error::Invalid);
            }
            for (column, reference) in foreign.columns.iter().zip(foreign.references) {
                let source = schema
                    .columns
                    .iter()
                    .find(|(c, _)| c == column)
                    .ok_or(Error::Invalid)?;
                let dest = target
                    .columns
                    .iter()
                    .find(|(c, _)| c == reference)
                    .ok_or(Error::Invalid)?;
                if source.1 != dest.1 {
                    return Err(Error::Invalid);
                }
            }
        }
    }
    Ok(())
}
fn kind(value: &Value) -> Kind {
    match value {
        Value::Text(_) => Kind::Text,
        Value::Integer(_) => Kind::Integer,
        Value::Bytes(_) => Kind::Bytes,
    }
}
fn validate_transaction(
    schemas: &BTreeMap<Table, Schema>,
    transaction: &Transaction,
) -> Result<(), Error> {
    let filter = |table: Table, predicates: &[Predicate]| -> Result<(), Error> {
        let schema = schemas.get(&table).ok_or(Error::Invalid)?;
        if predicates
            .iter()
            .any(|p| !schema.columns.contains(&(p.column, kind(&p.value))))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    };
    let query = |q: &Query| -> Result<(), Error> {
        filter(q.table, &q.filter)?;
        if q.order
            .iter()
            .any(|column| !schemas[&q.table].columns.iter().any(|(c, _)| c == column))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    };
    for guard in &transaction.guards {
        query(&guard.query)?;
    }
    for statement in &transaction.statements {
        match statement {
            Statement::Select(q) => query(q)?,
            Statement::Delete {
                table,
                filter: predicates,
            } => filter(*table, predicates)?,
            Statement::Update {
                table,
                filter: predicates,
                changes,
            } => {
                filter(*table, predicates)?;
                let schema = &schemas[table];
                if changes.is_empty()
                    || changes.iter().any(|(key, value)| {
                        schema.primary.contains(&key.as_str())
                            || !schema
                                .columns
                                .iter()
                                .any(|(c, t)| *c == key && *t == kind(value))
                    })
                {
                    return Err(Error::Invalid);
                }
            }
            Statement::Insert { table, row } => {
                let schema = schemas.get(table).ok_or(Error::Invalid)?;
                if row.len() != schema.columns.len()
                    || schema
                        .columns
                        .iter()
                        .any(|(column, ty)| row.get(*column).is_none_or(|value| kind(value) != *ty))
                {
                    return Err(Error::Invalid);
                }
            }
        }
    }
    Ok(())
}
fn matches(row: &Row, predicates: &[Predicate]) -> bool {
    predicates.iter().all(|p| match p.compare {
        Compare::Eq => row[p.column] == p.value,
        Compare::Ne => row[p.column] != p.value,
        Compare::Gt => row[p.column] > p.value,
        Compare::Le => row[p.column] <= p.value,
    })
}
fn validate_data(
    data: &BTreeMap<Table, Rows>,
    schemas: &BTreeMap<Table, Schema>,
) -> Result<(), Error> {
    for (table, rows) in data {
        let schema = &schemas[table];
        for (i, row) in rows.iter().enumerate() {
            for keys in core::iter::once(schema.primary).chain(
                schema
                    .indexes
                    .iter()
                    .filter(|i| i.unique)
                    .map(|i| i.columns),
            ) {
                if rows[..i]
                    .iter()
                    .any(|other| keys.iter().all(|key| other.get(*key) == row.get(*key)))
                {
                    return Err(Error::Constraint);
                }
            }
            for foreign in schema.foreign {
                if !data[&foreign.target].iter().any(|target| {
                    foreign
                        .columns
                        .iter()
                        .zip(foreign.references)
                        .all(|(c, r)| row.get(*c) == target.get(*r))
                }) {
                    return Err(Error::Constraint);
                }
            }
        }
    }
    Ok(())
}
fn order<'a>(schema: &'a Schema, query: &'a Query) -> Vec<&'static str> {
    query
        .order
        .iter()
        .copied()
        .chain(schema.primary.iter().copied())
        .collect()
}
fn select_memory(
    data: &BTreeMap<Table, Rows>,
    schemas: &BTreeMap<Table, Schema>,
    query: &Query,
) -> Rows {
    let mut rows: Rows = data[&query.table]
        .iter()
        .filter(|row| matches(row, &query.filter))
        .cloned()
        .collect();
    let order = order(&schemas[&query.table], query);
    rows.sort_by(|a, b| {
        order
            .iter()
            .map(|c| &a[*c])
            .cmp(order.iter().map(|c| &b[*c]))
    });
    rows.truncate(query.limit as usize);
    rows
}
fn where_sql(filter: &[Predicate]) -> String {
    if filter.is_empty() {
        return String::new();
    }
    format!(
        " WHERE {}",
        filter
            .iter()
            .map(|p| format!(
                "{} {} ?",
                quote(p.column),
                match p.compare {
                    Compare::Eq => "=",
                    Compare::Ne => "!=",
                    Compare::Gt => ">",
                    Compare::Le => "<=",
                }
            ))
            .collect::<Vec<_>>()
            .join(" AND ")
    )
}
fn sql_value(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(v) => v.clone().into(),
        Value::Integer(v) => (*v).into(),
        Value::Bytes(v) => v.clone().into(),
    }
}
fn select_sql(
    db: &Connection,
    schemas: &BTreeMap<Table, Schema>,
    query: &Query,
) -> Result<Rows, Error> {
    let schema = &schemas[&query.table];
    let cols: Vec<_> = schema.columns.iter().map(|(c, _)| *c).collect();
    db.prepare(&format!(
        "SELECT {} FROM {}{} ORDER BY {} LIMIT {}",
        columns(&cols),
        quote(&name(query.table)),
        where_sql(&query.filter),
        columns(&order(schema, query)),
        query.limit
    ))
    .map_err(sql_error)?
    .query_map(
        params_from_iter(query.filter.iter().map(|p| sql_value(&p.value))),
        |row| {
            schema
                .columns
                .iter()
                .enumerate()
                .map(|(i, (column, kind))| {
                    Ok((
                        (*column).into(),
                        match kind {
                            Kind::Text => Value::Text(row.get(i)?),
                            Kind::Integer => Value::Integer(row.get(i)?),
                            Kind::Bytes => Value::Bytes(row.get(i)?),
                        },
                    ))
                })
                .collect::<rusqlite::Result<Row>>()
        },
    )
    .map_err(sql_error)?
    .collect::<Result<Rows, _>>()
    .map_err(sql_error)
}
fn sql_error(error: rusqlite::Error) -> Error {
    if matches!(error, rusqlite::Error::SqliteFailure(ref e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation)
    {
        Error::Constraint
    } else {
        Error::Unavailable
    }
}
