use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};
use serde_json::Value;
use snap_http::Response;
use snap_store::{
    Guard, Kind, Predicate as P, Query, Row, Rows, Schema, Statement as S, Store, Table,
    Transaction,
};
pub const SESSIONS: Table = Table {
    namespace: "chatty",
    name: "sessions",
};
pub const ATTEMPTS: Table = Table {
    namespace: "chatty",
    name: "attempts",
};
pub const THREADS: Table = Table {
    namespace: "chatty",
    name: "threads",
};
pub const TURNS: Table = Table {
    namespace: "chatty",
    name: "turns",
};
pub const SETTINGS: Table = Table {
    namespace: "chatty",
    name: "settings",
};
pub const OWNERS: Table = Table {
    namespace: "chatty",
    name: "owners",
};
pub fn schemas() -> Vec<Schema> {
    vec![
        Schema {
            table: OWNERS,
            columns: &[("id", Kind::Text), ("threads", Kind::Integer)],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: SESSIONS,
            columns: &[
                ("id", Kind::Text),
                ("owner", Kind::Text),
                ("data", Kind::Text),
                ("expires", Kind::Integer),
                ("version", Kind::Integer),
                ("refreshing", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: ATTEMPTS,
            columns: &[
                ("id", Kind::Text),
                ("binding", Kind::Text),
                ("data", Kind::Text),
                ("expires", Kind::Integer),
                ("processing", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: THREADS,
            columns: &[
                ("id", Kind::Text),
                ("owner", Kind::Text),
                ("title", Kind::Text),
                ("effort", Kind::Text),
                ("created", Kind::Integer),
                ("updated", Kind::Integer),
                ("revision", Kind::Integer),
                ("active", Kind::Text),
            ],
            primary: &["id"],
            indexes: &[snap_store::Index {
                columns: &["owner", "updated", "id"],
                unique: false,
            }],
            foreign: &[],
            legacy_name: None,
        },
        Schema {
            table: TURNS,
            columns: &[
                ("id", Kind::Text),
                ("thread_id", Kind::Text),
                ("request_id", Kind::Text),
                ("seq", Kind::Integer),
                ("user", Kind::Text),
                ("output", Kind::Text),
                ("text", Kind::Text),
                ("summary", Kind::Text),
                ("tools", Kind::Text),
                ("usage", Kind::Text),
                ("status", Kind::Text),
                ("error", Kind::Text),
                ("created", Kind::Integer),
                ("updated", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[
                snap_store::Index {
                    columns: &["thread_id", "request_id"],
                    unique: true,
                },
                snap_store::Index {
                    columns: &["thread_id", "seq"],
                    unique: false,
                },
            ],
            foreign: &[snap_store::ForeignKey {
                columns: &["thread_id"],
                target: THREADS,
                references: &["id"],
            }],
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
pub fn unavailable() -> Response {
    Response::error(503, "unavailable", "Chatty is temporarily unavailable")
}
pub fn conflict() -> Response {
    Response::error(
        409,
        "conflict",
        "The operation changed; reload to see its current state",
    )
}
pub fn login_required() -> Response {
    Response::error(401, "login_required", "Sign in with Authy")
}
pub fn row(fields: &[(&str, snap_store::Value)]) -> Row {
    fields
        .iter()
        .map(|(k, v)| ((*k).into(), v.clone()))
        .collect()
}
pub fn text(r: &Row, key: &str) -> Result<String, Response> {
    match r.get(key) {
        Some(snap_store::Value::Text(s)) => Ok(s.clone()),
        _ => Err(unavailable()),
    }
}
pub fn number(r: &Row, key: &str) -> Result<i64, Response> {
    match r.get(key) {
        Some(snap_store::Value::Integer(n)) => Ok(*n),
        _ => Err(unavailable()),
    }
}
pub fn json(r: &Row, key: &str) -> Result<Value, Response> {
    serde_json::from_str(&text(r, key)?).map_err(|_| unavailable())
}
pub fn id(table: Table, id: &str) -> Query {
    Query::new(table).matching(vec![P::eq("id", id)]).limit(1)
}
pub fn live(table: Table, key: &str, now: u64) -> Query {
    let mut q = id(table, key);
    q.filter.push(P::gt("expires", now as i64));
    q
}
pub fn guard(query: Query) -> Guard {
    Guard {
        query,
        exists: true,
    }
}
pub fn delete(table: Table, key: &str) -> S {
    S::Delete {
        table,
        filter: vec![P::eq("id", key)],
    }
}
pub async fn tx(
    store: &impl Store,
    guards: Vec<Guard>,
    statements: Vec<S>,
) -> Result<Vec<Rows>, Response> {
    store
        .transaction(Transaction { guards, statements })
        .await
        .map_err(|e| match e {
            snap_store::Error::Conflict | snap_store::Error::Constraint => conflict(),
            _ => unavailable(),
        })
}
pub async fn read(store: &impl Store, query: Query) -> Result<Option<Row>, Response> {
    Ok(tx(store, vec![], vec![S::Select(query)])
        .await?
        .remove(0)
        .pop())
}
pub fn stringify(value: &Value) -> snap_store::Value {
    value.to_string().into()
}

/// Called once at host initialization. No model request or file write is replayed.
/// A consumed refresh exchange with unknown response requires a new Authy login.
pub async fn recover(store: &impl Store, now: u64) -> Result<(), Response> {
    tx(
        store,
        vec![],
        vec![
            S::Update {
                table: TURNS,
                filter: vec![P::eq("status", "running")],
                changes: row(&[
                    ("status", "interrupted".into()),
                    (
                        "error",
                        "The host restarted during this reply. Send a new message to continue."
                            .into(),
                    ),
                    ("updated", (now as i64).into()),
                ]),
            },
            S::Update {
                table: THREADS,
                filter: vec![],
                changes: row(&[("active", "".into())]),
            },
            S::Delete {
                table: SESSIONS,
                filter: vec![P::eq("refreshing", 1i64)],
            },
            S::Delete {
                table: ATTEMPTS,
                filter: vec![P::eq("processing", 1i64)],
            },
        ],
    )
    .await?;
    Ok(())
}
