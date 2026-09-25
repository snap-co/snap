//! Shared validation of the bounded relational contract, before any backend work.
use crate::*;
use alloc::format;

pub fn physical_name(table: Table) -> String {
    format!("{}_{}", table.namespace, table.name)
}

pub fn schemas(schemas: &[Schema]) -> Result<(), Error> {
    let valid = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    let mut names = Vec::new();
    for schema in schemas {
        let physical = physical_name(schema.table);
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

pub fn transaction(
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
