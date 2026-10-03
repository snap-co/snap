use snap_store::Row;
use snap_store_sqlite::Sqlite;
use snap_transport::{Command, Error, Event, Invocation, Response, json};
use testy_server::store::{Host, migration};

#[test]
fn signup_returns_a_miss_then_a_separate_request_commits_all_modules() {
    let mut host = Host::new(Sqlite::memory(&[migration()]).unwrap());
    for table in [
        "signup.accounts",
        "signup.grants",
        "signup.documents",
        "signup.outbox",
    ] {
        host.store.load(table).unwrap();
    }
    let command = |id| Command::Request {
        bearer: Some(testy::BEARER.into()),
        invocation: Invocation {
            id,
            operation: "accounts.create".into(),
            input: json!({"id": 7, "email": "alice@example.test"}),
        },
    };
    assert_eq!(
        host.exchange(command(1)),
        vec![
            Response::Event(Event::Accepted { id: 1 }),
            Response::Event(Event::Completed {
                id: 1,
                outcome: Err(Error::Application(
                    json!({"code": "StoreMiss", "table": "signup.policy", "index": "primary"})
                ))
            })
        ]
    );
    for table in [
        "signup.accounts",
        "signup.grants",
        "signup.documents",
        "signup.outbox",
    ] {
        assert!(
            host.store
                .run("verify rollback", |tx| tx.get(table, &[7.into()]))
                .unwrap()
                .value
                .is_none()
        );
    }
    assert_eq!(host.store.misses().count, 1);
    host.store.load("signup.policy").unwrap();
    assert_eq!(
        host.exchange(command(2)),
        vec![
            Response::Event(Event::Accepted { id: 2 }),
            Response::Event(Event::Completed {
                id: 2,
                outcome: Err(Error::Application(json!({"code": "NotFound"})))
            }),
        ]
    );
    assert_eq!(
        host.store.misses().count,
        1,
        "known absence is not another miss"
    );
    host.store
        .run("configure policy", |tx| {
            tx.insert(
                "signup.policy",
                Row::from([("id".into(), 0.into()), ("enabled".into(), 1.into())]),
            )
        })
        .unwrap();
    assert_eq!(
        host.exchange(command(3)),
        vec![
            Response::Event(Event::Accepted { id: 3 }),
            Response::Event(Event::Completed {
                id: 3,
                outcome: Ok(json!({"created": 7}))
            })
        ]
    );
    for table in [
        "signup.accounts",
        "signup.grants",
        "signup.documents",
        "signup.outbox",
    ] {
        assert!(
            host.store
                .run("verify commit", |tx| tx.get(table, &[7.into()]))
                .unwrap()
                .value
                .is_some()
        );
    }
}
