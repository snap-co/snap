//! One Store spans the namespaces registered in one SQLite Durable Object.
//! Transactions never await between guards and statements. A successful future
//! also waits for storage.sync(), not just the in-memory SQLite commit.
use snap_store::{
    Compare, Error, Kind, Predicate, Query, Row, Rows, Schema, Statement, Table, Transaction,
    Value, validation,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

// The released workers-rs Storage wrapper does not expose transactionSync/sync.
// Keep this small, checked JS bridge at the platform edge. No unsafe Send shim.
#[wasm_bindgen(inline_js = "
export function snapSql(storage, query, bindings) {
  return JSON.stringify(storage.sql.exec(query, ...bindings).raw().toArray());
}
export function snapTransaction(storage, callback) { return storage.transactionSync(callback); }
export async function snapSync(storage) { await storage.sync(); }
")]
extern "C" {
    #[wasm_bindgen(catch, js_name = snapSql)]
    fn sql(storage: &JsValue, query: &str, bindings: &js_sys::Array) -> Result<String, JsValue>;
    #[wasm_bindgen(catch, js_name = snapTransaction)]
    fn atomic(storage: &JsValue, callback: &js_sys::Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = snapSync)]
    async fn sync(storage: &JsValue) -> Result<(), JsValue>;
}

#[derive(Clone)]
pub struct Store {
    storage: JsValue,
    schemas: Rc<BTreeMap<Table, Schema>>,
}

impl Store {
    /// `storage` must be this Durable Object's own DurableObjectStorage binding.
    /// Register all participating namespaces together before serving requests.
    /// A fresh Workers database is supported; native-file migration is not implied.
    pub async fn new(storage: JsValue, schemas: &[Schema]) -> Result<Self, Error> {
        validation::schemas(schemas)?;
        let store = Self {
            storage,
            schemas: Rc::new(schemas.iter().cloned().map(|s| (s.table, s)).collect()),
        };
        let register = store.clone();
        store.atomic(move || register.register())?;
        sync(&store.storage).await.map_err(|_| Error::Unavailable)?;
        Ok(store)
    }

    fn atomic<T: 'static>(
        &self,
        work: impl FnOnce() -> Result<T, Error> + 'static,
    ) -> Result<T, Error> {
        let result = Rc::new(RefCell::new(None));
        let slot = result.clone();
        let callback = Closure::once_assert_unwind_safe(move || -> Result<JsValue, JsValue> {
            let outcome = work();
            let failed = outcome.is_err();
            *slot.borrow_mut() = Some(outcome);
            if failed {
                Err(JsValue::from_str("Snap transaction rolled back"))
            } else {
                Ok(JsValue::UNDEFINED)
            }
        });
        let status = atomic(&self.storage, callback.as_ref().unchecked_ref());
        let result = result
            .borrow_mut()
            .take()
            .unwrap_or(Err(Error::Unavailable));
        match (status, result) {
            (Ok(_), result) => result,
            (Err(_), Err(error)) => Err(error),
            _ => Err(Error::Unavailable),
        }
    }

    fn exec(&self, query: &str, params: &[Value]) -> Result<Vec<Vec<serde_json::Value>>, Error> {
        let bindings = js_sys::Array::new();
        for value in params {
            bindings.push(&match value {
                Value::Text(v) => JsValue::from_str(v),
                // Bind as decimal text, explicitly CAST in SQL. No f64 round trip.
                Value::Integer(v) => JsValue::from_str(&v.to_string()),
                Value::Bytes(v) => js_sys::Uint8Array::from(v.as_slice()).into(),
            });
        }
        let text = sql(&self.storage, query, &bindings).map_err(|error| {
            let message = js_sys::Reflect::get(&error, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
                .unwrap_or_default();
            if message.contains("SQLITE_CONSTRAINT") || message.contains("constraint failed") {
                Error::Constraint
            } else {
                Error::Unavailable
            }
        })?;
        serde_json::from_str(&text).map_err(|_| Error::Unavailable)
    }

    fn register(&self) -> Result<(), Error> {
        self.exec("CREATE TABLE IF NOT EXISTS snap_store_schemas (physical TEXT PRIMARY KEY, logical TEXT NOT NULL, shape TEXT NOT NULL)", &[])?;
        for schema in self.schemas.values() {
            if let Some(legacy) = schema.legacy_name
                && !self
                    .exec(
                        "SELECT name FROM sqlite_master WHERE name=? COLLATE NOCASE",
                        &[legacy.into()],
                    )?
                    .is_empty()
            {
                return Err(Error::Invalid);
            }
            let physical = validation::physical_name(schema.table);
            let logical = format!("{}.{}", schema.table.namespace, schema.table.name);
            let shape = format!(
                "{:?}/{:?}/{:?}/{:?}",
                schema.columns, schema.primary, schema.indexes, schema.foreign
            );
            let registered = self.exec(
                "SELECT logical,shape FROM snap_store_schemas WHERE physical=? COLLATE NOCASE",
                &[physical.clone().into()],
            )?;
            if let Some(row) = registered.first() {
                if row
                    != &[
                        serde_json::Value::String(logical),
                        serde_json::Value::String(shape),
                    ]
                {
                    return Err(Error::Invalid);
                }
                continue;
            }
            // Never claim a table created outside this schema registry.
            if !self
                .exec(
                    "SELECT name FROM sqlite_master WHERE name=? COLLATE NOCASE",
                    &[physical.clone().into()],
                )?
                .is_empty()
            {
                return Err(Error::Invalid);
            }
            let fields = schema
                .columns
                .iter()
                .map(|(c, k)| {
                    format!(
                        "{} {} NOT NULL",
                        quote(c),
                        match k {
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
                        table(f.target),
                        columns(f.references)
                    )
                })
                .collect::<String>();
            self.exec(
                &format!(
                    "CREATE TABLE {} ({fields}, PRIMARY KEY ({}){foreign})",
                    quote(&physical),
                    columns(schema.primary)
                ),
                &[],
            )?;
            for (i, index) in schema.indexes.iter().enumerate() {
                self.exec(
                    &format!(
                        "CREATE {} INDEX {} ON {} ({})",
                        if index.unique { "UNIQUE" } else { "" },
                        quote(&format!("{physical}_i{i}")),
                        quote(&physical),
                        columns(index.columns)
                    ),
                    &[],
                )?;
            }
            self.exec(
                "INSERT INTO snap_store_schemas VALUES (?,?,?)",
                &[physical.into(), logical.into(), shape.into()],
            )?;
        }
        Ok(())
    }

    fn select(&self, query: &Query) -> Result<Rows, Error> {
        let schema = &self.schemas[&query.table];
        let fields = schema
            .columns
            .iter()
            .map(|(column, kind)| match kind {
                Kind::Bytes => format!("hex({})", quote(column)),
                _ => format!("CAST({} AS TEXT)", quote(column)),
            })
            .collect::<Vec<_>>()
            .join(",");
        let order: Vec<_> = query.order.iter().chain(schema.primary).copied().collect();
        let rows = self.exec(
            &format!(
                "SELECT {fields} FROM {}{} ORDER BY {} LIMIT {}",
                table(query.table),
                filter(&query.filter),
                columns(&order),
                query.limit
            ),
            &values(&query.filter),
        )?;
        rows.into_iter()
            .map(|row| {
                if row.len() != schema.columns.len() {
                    return Err(Error::Unavailable);
                }
                schema
                    .columns
                    .iter()
                    .zip(row)
                    .map(|((column, kind), value)| {
                        let text = value.as_str().ok_or(Error::Unavailable)?;
                        let value = match kind {
                            Kind::Text => Value::Text(text.into()),
                            Kind::Integer => {
                                Value::Integer(text.parse().map_err(|_| Error::Unavailable)?)
                            }
                            Kind::Bytes => Value::Bytes(
                                (0..text.len())
                                    .step_by(2)
                                    .map(|i| {
                                        text.get(i..i + 2)
                                            .and_then(|s| u8::from_str_radix(s, 16).ok())
                                            .ok_or(Error::Unavailable)
                                    })
                                    .collect::<Result<_, _>>()?,
                            ),
                        };
                        Ok(((*column).into(), value))
                    })
                    .collect::<Result<Row, Error>>()
            })
            .collect()
    }

    fn execute(&self, transaction: Transaction) -> Result<Vec<Rows>, Error> {
        for guard in transaction.guards {
            if !self.select(&guard.query)?.is_empty() != guard.exists {
                return Err(Error::Conflict);
            }
        }
        let mut results = Vec::new();
        for statement in transaction.statements {
            match statement {
                Statement::Select(query) => {
                    results.push(self.select(&query)?);
                    continue;
                }
                Statement::Insert { table: target, row } => {
                    let names: Vec<_> = row.keys().map(String::as_str).collect();
                    let slots = row.values().map(slot).collect::<Vec<_>>().join(",");
                    self.exec(
                        &format!(
                            "INSERT INTO {} ({}) VALUES ({slots})",
                            table(target),
                            columns(&names)
                        ),
                        &row.into_values().collect::<Vec<_>>(),
                    )?;
                }
                Statement::Update {
                    table: target,
                    filter: predicates,
                    changes,
                } => {
                    let sets = changes
                        .iter()
                        .map(|(c, v)| format!("{}={}", quote(c), slot(v)))
                        .collect::<Vec<_>>()
                        .join(",");
                    let params: Vec<_> = changes.into_values().chain(values(&predicates)).collect();
                    self.exec(
                        &format!("UPDATE {} SET {sets}{}", table(target), filter(&predicates)),
                        &params,
                    )?;
                }
                Statement::Delete {
                    table: target,
                    filter: predicates,
                } => {
                    self.exec(
                        &format!("DELETE FROM {}{}", table(target), filter(&predicates)),
                        &values(&predicates),
                    )?;
                }
            }
            results.push(Vec::new());
        }
        Ok(results)
    }
}

impl snap_store::Store for Store {
    async fn transaction(&self, transaction: Transaction) -> Result<Vec<Rows>, Error> {
        validation::transaction(&self.schemas, &transaction)?;
        let store = self.clone();
        let results = self.atomic(move || store.execute(transaction))?;
        sync(&self.storage).await.map_err(|_| Error::Unavailable)?;
        Ok(results)
    }
}

fn quote(name: &str) -> String {
    format!("\"{name}\"")
}
fn table(t: Table) -> String {
    quote(&validation::physical_name(t))
}
fn columns(names: &[&str]) -> String {
    names.iter().map(|c| quote(c)).collect::<Vec<_>>().join(",")
}
fn slot(value: &Value) -> &'static str {
    if matches!(value, Value::Integer(_)) {
        "CAST(? AS INTEGER)"
    } else {
        "?"
    }
}
fn values(predicates: &[Predicate]) -> Vec<Value> {
    predicates.iter().map(|p| p.value.clone()).collect()
}
fn filter(predicates: &[Predicate]) -> String {
    if predicates.is_empty() {
        return String::new();
    }
    format!(
        " WHERE {}",
        predicates
            .iter()
            .map(|p| format!(
                "{} {} {}",
                quote(p.column),
                match p.compare {
                    Compare::Eq => "=",
                    Compare::Ne => "!=",
                    Compare::Gt => ">",
                    Compare::Le => "<=",
                },
                slot(&p.value)
            ))
            .collect::<Vec<_>>()
            .join(" AND ")
    )
}
