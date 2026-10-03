//! Generated admission/lifetime histories through the portable Document runtime.
use hegel::{TestCase, generators as gs};
use snap_access::{Actor, Audience, ChangeSet, GrantChange, Resource, Role};
use snap_document::runtime::Runtime as Host;
use snap_document::{Definition, Intent, Mutation, Registry, Snapshot, server::Document};
use snap_transport::{Command, Event, Invocation, Response, json, server::Config};
use std::sync::{Arc, Mutex};

const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";

fn document() -> Document {
    Document::new(
        Registry::new(vec![Definition {
            kind: "counter".into(),
            version: "1".into(),
            validate: |v| v.is_i64(),
            mutations: vec![Mutation {
                name: "add".into(),
                minimum: Role::Editor,
                guard: None,
                apply: |value, args, _| Ok(json!(value.as_i64().unwrap() + args.as_i64().unwrap())),
            }],
        }])
        .unwrap(),
    )
}

fn grant(tx: &mut snap_store::Transaction<'_>, enabled: bool) -> Result<(), snap_store::Error> {
    let mut changes = ChangeSet::new(Actor::system());
    changes.grants.push(GrantChange {
        resource: Resource::new("document", ID).unwrap(),
        identity: "bob".into(),
        role: enabled.then_some(Role::Editor),
    });
    snap_document::access::vocabulary()
        .change(tx, &changes)
        .map(|_| ())
}

#[hegel::test]
fn accepted_authority_dedup_and_draining_match_committed_effects(tc: TestCase) {
    let mut migrations: Vec<snap_store::migration::Migration> = vec![
        toml::from_str(snap_access::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::MIGRATION).unwrap(),
        toml::from_str(snap_document::server::LIFECYCLE_MIGRATION).unwrap(),
    ];
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    store
        .run("fixture", |tx| {
            document().create(
                tx,
                &Snapshot {
                    id: ID.into(),
                    kind: "counter".into(),
                    version: "1".into(),
                    revision: 1,
                    value: json!(0),
                },
                Audience::Restricted,
                "alice",
            )
        })
        .unwrap();
    let effects = Arc::new(Mutex::new(Vec::new()));
    let observed = effects.clone();
    let mut host = Host::new(
        store,
        document(),
        Arc::new(|_, bearer| Ok(bearer.into())),
        Config {
            reconnect_ms: 10,
            capacity: 100,
        },
        "properties".into(),
    )
    .with_controller(
        "counter",
        Box::new(move |_, snapshot| {
            observed.lock().unwrap().push(snapshot.value);
            Ok(())
        }),
    );
    let mut expected = 0_i64;
    let mut committed = Vec::new();
    let steps = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    for step in 0..steps {
        let allowed = tc.draw(gs::booleans());
        let amount = tc.draw(gs::integers::<i64>().min_value(1).max_value(8));
        let duplicate = tc.draw(gs::booleans());
        let close = tc.draw(gs::booleans());
        let carrier = tc.draw(gs::booleans());
        let revoke = tc.draw(gs::booleans());
        tc.note(&format!("step={step} allowed={allowed} amount={amount} duplicate={duplicate} close={close} carrier={carrier} revoke={revoke}"));
        host.transact("grant", |tx| grant(tx, allowed)).unwrap();
        let peer = host.open().unwrap();
        host.submit(
            peer,
            Command::Connect {
                bearer: "bob".into(),
                client_id: format!("client-{step}"),
            },
            step as u64 * 100,
        )
        .unwrap();
        host.drain(peer).unwrap();
        let command = Command::Invoke(Invocation {
            id: 1,
            operation: "document.mutate".into(),
            input: serde_json::to_value(Intent {
                id: 1,
                document: ID.into(),
                version: "1".into(),
                mutation: "add".into(),
                args: json!(amount),
            })
            .unwrap(),
        });
        host.submit(peer, command.clone(), step as u64 * 100)
            .unwrap();
        let first = host.drain(peer).unwrap();
        let accepted = first
            .iter()
            .any(|response| matches!(response, Response::Event(Event::Accepted { id: 1 })));
        assert_eq!(accepted, allowed);
        if duplicate {
            host.submit(peer, command, step as u64 * 100).unwrap();
        }
        if carrier {
            let control = host.carrier_control(peer).unwrap();
            if close {
                control.close(step as u64 * 100);
            }
            control.detach(step as u64 * 100);
            host.tick(step as u64 * 100 + 10);
        } else if close {
            host.submit(peer, Command::Close, step as u64 * 100)
                .unwrap();
        } else {
            host.lost(peer, step as u64 * 100);
            host.tick(step as u64 * 100 + 10);
        }
        // Revocation must wait behind accepted work, even without a socket.
        if revoke {
            host.transact("revoke", |tx| grant(tx, false)).unwrap();
        }
        while host.step() {}
        if allowed {
            expected += amount;
            committed.push(json!(expected));
        }
        let actual = host
            .transact("inspect", |tx| document().read(tx, ID, Some("alice")))
            .unwrap();
        assert_eq!(actual.value, json!(expected));
        assert_eq!(*effects.lock().unwrap(), committed);
        host.lost(peer, step as u64 * 100 + 20);
    }
}
