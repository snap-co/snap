//! Transactional memory backend. Snapshots are isolated copies, not shared resets.
use snap_store::{
    Compare, Error, Predicate, Query, Row, Rows, Schema, Statement, Table, Transaction,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

#[derive(Clone)]
pub struct Database {
    data: BTreeMap<Table, Rows>,
    schemas: BTreeMap<Table, Schema>,
}
impl Database {
    pub fn new(schemas: &[Schema]) -> Result<Self, Error> {
        snap_store::validation::schemas(schemas)?;
        Ok(Self {
            data: schemas.iter().map(|s| (s.table, Vec::new())).collect(),
            schemas: schemas.iter().cloned().map(|s| (s.table, s)).collect(),
        })
    }
    pub fn execute(&mut self, transaction: Transaction) -> Result<Vec<Rows>, Error> {
        snap_store::validation::transaction(&self.schemas, &transaction)?;
        let mut next = self.data.clone();
        for guard in transaction.guards {
            if !select(&next, &self.schemas, &guard.query).is_empty() != guard.exists {
                return Err(Error::Conflict);
            }
        }
        let mut results = Vec::new();
        for statement in transaction.statements {
            match statement {
                Statement::Select(query) => {
                    results.push(select(&next, &self.schemas, &query));
                    continue;
                }
                Statement::Delete { table, filter } => next
                    .get_mut(&table)
                    .unwrap()
                    .retain(|row| !matches(row, &filter)),
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
                }
                Statement::Insert { table, row } => next.get_mut(&table).unwrap().push(row),
            }
            validate_data(&next, &self.schemas)?;
            results.push(Vec::new());
        }
        self.data = next;
        Ok(results)
    }
}

#[derive(Clone)]
pub struct Store(Rc<RefCell<Database>>);
impl Store {
    pub fn new(schemas: &[Schema]) -> Result<Self, Error> {
        Ok(Self::from_snapshot(Database::new(schemas)?))
    }
    pub fn from_snapshot(database: Database) -> Self {
        Self(Rc::new(RefCell::new(database)))
    }
    /// Snapshot storage only at a quiescent point; tasks/crypto/clock are not copied.
    pub fn snapshot(&self) -> Database {
        self.0.borrow().clone()
    }
}
impl snap_store::Store for Store {
    async fn transaction(&self, transaction: Transaction) -> Result<Vec<Rows>, Error> {
        self.0.borrow_mut().execute(transaction)
    }
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
fn select(data: &BTreeMap<Table, Rows>, schemas: &BTreeMap<Table, Schema>, query: &Query) -> Rows {
    let mut rows: Rows = data[&query.table]
        .iter()
        .filter(|r| matches(r, &query.filter))
        .cloned()
        .collect();
    let order: Vec<_> = query
        .order
        .iter()
        .chain(schemas[&query.table].primary.iter())
        .copied()
        .collect();
    rows.sort_by(|a, b| {
        order
            .iter()
            .map(|c| &a[*c])
            .cmp(order.iter().map(|c| &b[*c]))
    });
    rows.truncate(query.limit as usize);
    rows
}
