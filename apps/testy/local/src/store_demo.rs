use snap_sqlite::Sqlite;
use snap_store::Row;
use snap_transport::{Command, Invocation, json};
use testy_local::store::Host;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".snap/testy-store.sqlite".into());
    let id: i64 = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "1".into())
        .parse()?;
    let mut host = Host::new(Sqlite::open(std::path::Path::new(&path))?);
    let command = |invocation_id| Command::Request {
        bearer: Some(testy::BEARER.into()),
        invocation: Invocation {
            id: invocation_id,
            operation: "accounts.create".into(),
            input: json!({"id": id, "email": format!("user-{id}@example.test")}),
        },
    };
    println!("First explicit request: {:?}", host.exchange(command(1)));
    println!("Misses recorded: {}", host.store.misses().count);
    // A deliberate host preloading decision, NOT a retry policy in Store.
    for table in testy::store::TABLES {
        host.store.load(table)?;
    }
    host.store.run("configure demo policy", |tx| {
        if tx.get("signup.policy", &[0.into()])?.is_none() {
            tx.insert(
                "signup.policy",
                Row::from([("id".into(), 0.into()), ("enabled".into(), 1.into())]),
            )?;
        }
        Ok(())
    })?;
    println!("Second explicit request: {:?}", host.exchange(command(2)));
    let outbox = host
        .store
        .run("inspect outbox", |tx| tx.get("signup.outbox", &[id.into()]))?;
    println!("Committed notification intent: {:?}", outbox.value);
    Ok(())
}
