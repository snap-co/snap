//! Issuer-owned records. Raw bearer tokens and authorization codes never enter Store.
use alloc::{string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_http::Response;
use snap_store::{
    Guard, Kind, Predicate as P, Query, Row, Rows, Schema, Statement as S, Store, Table,
    Transaction,
};

pub const FLOWS: Table = Table {
    namespace: "snap_oidc",
    name: "flows",
};
pub const GRANTS: Table = Table {
    namespace: "snap_oidc",
    name: "grants",
};
pub const TOKENS: Table = Table {
    namespace: "snap_oidc",
    name: "tokens",
};
pub const SETTINGS: Table = Table {
    namespace: "snap_oidc",
    name: "settings",
};
pub fn schemas() -> [Schema; 4] {
    [
        Schema {
            table: FLOWS,
            columns: &[
                ("id", Kind::Text),
                ("kind", Kind::Text),
                ("data", Kind::Text),
                ("expires", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: GRANTS,
            columns: &[
                ("id", Kind::Text),
                ("data", Kind::Text),
                ("active", Kind::Integer),
                ("expires", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: TOKENS,
            columns: &[
                ("id", Kind::Text),
                ("grant_id", Kind::Text),
                ("kind", Kind::Text),
                ("used", Kind::Integer),
                ("expires", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: SETTINGS,
            columns: &[("key", Kind::Text), ("value", Kind::Text)],
            primary: &["key"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
    ]
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Authorization {
    #[serde(default)]
    pub grant: String,
    pub client: String,
    pub redirect: String,
    pub scope: String,
    pub state: String,
    pub nonce: String,
    pub challenge: String,
    pub subject: String,
    pub session: String,
    pub auth_time: u64,
}
pub fn row(fields: &[(&str, snap_store::Value)]) -> Row {
    fields
        .iter()
        .map(|(k, v)| ((*k).into(), v.clone()))
        .collect()
}
pub fn text(row: &Row, key: &str) -> Result<String, Response> {
    match row.get(key) {
        Some(snap_store::Value::Text(s)) => Ok(s.clone()),
        _ => Err(crate::unavailable()),
    }
}
pub fn integer(row: &Row, key: &str) -> Result<i64, Response> {
    match row.get(key) {
        Some(snap_store::Value::Integer(n)) => Ok(*n),
        _ => Err(crate::unavailable()),
    }
}
pub fn authorization(row: &Row) -> Result<Authorization, Response> {
    serde_json::from_str(&text(row, "data")?).map_err(|_| crate::unavailable())
}
pub fn live(table: Table, id: &str, now: u64) -> Query {
    Query::new(table)
        .matching(vec![P::eq("id", id), P::gt("expires", now as i64)])
        .limit(1)
}
pub fn guard(query: Query) -> Guard {
    Guard {
        query,
        exists: true,
    }
}
pub async fn transaction(
    store: &impl Store,
    guards: Vec<Guard>,
    statements: Vec<S>,
) -> Result<Vec<Rows>, Response> {
    store
        .transaction(Transaction { guards, statements })
        .await
        .map_err(|e| match e {
            snap_store::Error::Conflict | snap_store::Error::Constraint => crate::invalid_grant(),
            _ => crate::unavailable(),
        })
}
pub async fn read(store: &impl Store, query: Query) -> Result<Option<Row>, Response> {
    Ok(transaction(store, vec![], vec![S::Select(query)])
        .await?
        .remove(0)
        .pop())
}
pub fn flow(id: &str, kind: &str, data: &Authorization, expires: u64) -> S {
    S::Insert {
        table: FLOWS,
        row: row(&[
            ("id", id.into()),
            ("kind", kind.into()),
            (
                "data",
                serde_json::to_string(data)
                    .expect("authorization serialization")
                    .into(),
            ),
            ("expires", (expires as i64).into()),
        ]),
    }
}
pub fn delete(table: Table, id: &str) -> S {
    S::Delete {
        table,
        filter: vec![P::eq("id", id)],
    }
}
pub fn token(id: &str, grant: &str, kind: &str, expires: u64) -> S {
    S::Insert {
        table: TOKENS,
        row: row(&[
            ("id", id.into()),
            ("grant_id", grant.into()),
            ("kind", kind.into()),
            ("used", 0i64.into()),
            ("expires", (expires as i64).into()),
        ]),
    }
}
