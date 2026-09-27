//! Ordered, explicit database-schema changes. This is unrelated to document JSON
//! schema evolution. Hosts translate these changes to their own DDL dialect.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub id: String,
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    CreateTable {
        table: Table,
    },
    DropTable {
        table: String,
    },
    AddColumn {
        table: String,
        column: Column,
        fill: Value,
    },
    RenameColumn {
        table: String,
        from: String,
        to: String,
    },
    CreateIndex {
        table: String,
        index: Index,
    },
    DropIndex {
        table: String,
        index: String,
    },
}

impl Migration {
    /// Validate against the prior catalog without changing it on any error.
    /// Foreign-key dependencies may be introduced together in one migration.
    pub fn apply(&self, prior: &Catalog) -> Result<Catalog, Error> {
        if !identifier(&self.id) || self.changes.is_empty() {
            return Err(Error::Invalid);
        }
        let mut catalog = prior.clone();
        for change in &self.changes {
            match change {
                Change::CreateTable { table } => {
                    if catalog.tables.iter().any(|t| t.name == table.name) {
                        return Err(Error::Invalid);
                    }
                    catalog.tables.push(table.clone());
                }
                Change::DropTable { table } => {
                    catalog.table(table)?;
                    catalog.tables.retain(|t| t.name != *table);
                }
                Change::AddColumn {
                    table,
                    column,
                    fill,
                } => {
                    if kind(fill) != column.kind {
                        return Err(Error::Invalid);
                    }
                    let target = catalog
                        .tables
                        .iter_mut()
                        .find(|t| t.name == *table)
                        .ok_or(Error::Invalid)?;
                    if target.columns.iter().any(|c| c.name == column.name) {
                        return Err(Error::Invalid);
                    }
                    target.columns.push(column.clone());
                }
                Change::RenameColumn { table, from, to } => {
                    let target = catalog
                        .tables
                        .iter_mut()
                        .find(|t| t.name == *table)
                        .ok_or(Error::Invalid)?;
                    if target.columns.iter().any(|c| c.name == *to) {
                        return Err(Error::Invalid);
                    }
                    target
                        .columns
                        .iter_mut()
                        .find(|c| c.name == *from)
                        .ok_or(Error::Invalid)?
                        .name = to.clone();
                    let rename = |names: &mut Vec<String>| {
                        for name in names {
                            if *name == *from {
                                *name = to.clone();
                            }
                        }
                    };
                    rename(&mut target.primary);
                    for index in &mut target.indexes {
                        rename(&mut index.columns);
                    }
                    for foreign in &mut target.foreign {
                        rename(&mut foreign.columns);
                    }
                    for target in &mut catalog.tables {
                        for foreign in &mut target.foreign {
                            if foreign.table == *table {
                                rename(&mut foreign.references);
                            }
                        }
                    }
                }
                Change::CreateIndex { table, index } => {
                    catalog
                        .tables
                        .iter_mut()
                        .find(|t| t.name == *table)
                        .ok_or(Error::Invalid)?
                        .indexes
                        .push(index.clone());
                }
                Change::DropIndex { table, index } => {
                    let target = catalog
                        .tables
                        .iter_mut()
                        .find(|t| t.name == *table)
                        .ok_or(Error::Invalid)?;
                    if !target.indexes.iter().any(|i| i.name == *index) {
                        return Err(Error::Invalid);
                    }
                    target.indexes.retain(|i| i.name != *index);
                }
            }
            // Validate local DDL dependencies at THIS step, not just the final
            // shape. Cross-table FK targets may still be introduced later.
            for table in &catalog.tables {
                table.validate_local()?;
            }
        }
        catalog.validate()?;
        Ok(catalog)
    }
}
