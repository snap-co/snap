use super::*;
use crate::Kind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub name: String,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub table: String,
    pub references: Vec<String>,
}

/// Names are module-qualified (`identity.accounts`). Every column is non-null.
/// Foreign keys target a complete primary key; deletes never cascade implicitly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub primary: Vec<String>,
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub foreign: Vec<ForeignKey>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub tables: Vec<Table>,
}

pub fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

pub fn kind(value: &Value) -> Kind {
    match value {
        Value::Text(_) => Kind::Text,
        Value::Integer(_) => Kind::Integer,
        Value::Bytes(_) => Kind::Bytes,
    }
}

impl Catalog {
    pub fn new(tables: Vec<Table>) -> Result<Self, Error> {
        let catalog = Self { tables };
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn table(&self, name: &str) -> Result<&Table, Error> {
        self.tables
            .iter()
            .find(|t| t.name == name)
            .ok_or(Error::Invalid)
    }

    pub fn validate(&self) -> Result<(), Error> {
        let mut names = BTreeSet::new();
        for table in &self.tables {
            table.validate_local()?;
            if !names.insert(&table.name) {
                return Err(Error::Invalid);
            }
            for foreign in &table.foreign {
                let target = self.table(&foreign.table)?;
                if foreign.references != target.primary
                    || foreign.columns.len() != foreign.references.len()
                {
                    return Err(Error::Invalid);
                }
                for (source, dest) in foreign.columns.iter().zip(&foreign.references) {
                    if table.column(source)?.kind != target.column(dest)?.kind {
                        return Err(Error::Invalid);
                    }
                }
            }
        }
        Ok(())
    }
}

impl Table {
    /// Validate one DDL step without requiring foreign target tables to have been
    /// created yet. Local columns MUST exist when keys/indexes are declared.
    pub(super) fn validate_local(&self) -> Result<(), Error> {
        let parts: Vec<_> = self.name.split('.').collect();
        if parts.len() != 2
            || parts[0] == "snap"
            || !parts.iter().all(|n| identifier(n))
            || self.columns.is_empty()
        {
            return Err(Error::Invalid);
        }
        let mut columns = BTreeSet::new();
        for column in &self.columns {
            if !identifier(&column.name) || !columns.insert(&column.name) {
                return Err(Error::Invalid);
            }
        }
        let valid_key = |key: &[String]| {
            !key.is_empty()
                && key.iter().all(|c| columns.contains(c))
                && key.iter().collect::<BTreeSet<_>>().len() == key.len()
        };
        if !valid_key(&self.primary) {
            return Err(Error::Invalid);
        }
        let mut indexes = BTreeSet::new();
        for index in &self.indexes {
            if !identifier(&index.name)
                || index.name == "primary"
                || !indexes.insert(&index.name)
                || !valid_key(&index.columns)
            {
                return Err(Error::Invalid);
            }
        }
        if self.foreign.iter().any(|f| !valid_key(&f.columns)) {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn column(&self, name: &str) -> Result<&Column, Error> {
        self.columns
            .iter()
            .find(|c| c.name == name)
            .ok_or(Error::Invalid)
    }
    pub fn index(&self, name: &str) -> Result<&[String], Error> {
        if name == "primary" {
            return Ok(&self.primary);
        }
        self.indexes
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.columns.as_slice())
            .ok_or(Error::Invalid)
    }
    pub fn validate_row(&self, row: &Row) -> Result<(), Error> {
        if row.len() != self.columns.len()
            || self
                .columns
                .iter()
                .any(|c| row.get(&c.name).is_none_or(|v| kind(v) != c.kind))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn key(&self, row: &Row) -> Vec<Value> {
        self.primary.iter().map(|c| row[c].clone()).collect()
    }
}
