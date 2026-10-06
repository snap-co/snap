//! Authy journeys through the portable client and real execution host. No
//! Document definitions or tables are needed for account-profile synchronization.
use authy::{Account, client::Profiles};
use serde_json::json;
use snap_identity::{Crypto, Identity};
use snap_store::{Error, replica::Publication};
use snap_transport::{Command, Event, Invocation, Operation, Response};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Default)]
struct Fake(Arc<AtomicU64>);
impl Crypto for Fake {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&(self.0.fetch_add(1, Ordering::SeqCst) + 1).to_be_bytes());
        Ok(bytes)
    }
    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        Ok(format!("fake:{password}"))
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        Ok(hash == format!("fake:{password}"))
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        secret.as_bytes().to_vec()
    }
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut all: Vec<_> = [
        snap_identity::MIGRATION,
        snap_access::MIGRATION,
        snap_store::resource::MIGRATION,
        authy::MIGRATION,
    ]
    .into_iter()
    .map(|m| toml::from_str(m).unwrap())
    .collect();
    all.sort_by(|a: &snap_store::migration::Migration, b| a.id.cmp(&b.id));
    all
}
type Host = snap_transport::host::Blocking<
    snap_store_sqlite::Sqlite,
    snap_transport::host::Controllers<
        snap_store_sqlite::Sqlite,
        snap_transport::replication::Replications,
    >,
>;

fn host(mut store: snap_store::Store<snap_store_sqlite::Sqlite>, crypto: Fake) -> Host {
    Identity::default().data().prepare(&mut store).unwrap();
    snap_store::resource::data().prepare(&mut store).unwrap();
    let replication = authy::replication();
    let mut registry = snap_transport::operation::Registry::default()
        .with_request(replication.operation())
        .with_request(authy::operations::edit_profile());
    let operations = snap_identity::operation::definitions(
        Identity::default(),
        {
            let crypto = crypto.clone();
            move || crypto.clone()
        },
        Some(snap_identity::operation::Enrollment {
            data: authy::enrollment_data(),
            initialize: Box::new(|tx, p, email| authy::initialize_account(tx, &p.identity, email)),
        }),
    );
    for definition in operations.preconnection {
        registry = registry.with_preconnection_request(definition);
    }
    for definition in operations.requests {
        registry = registry.with_request(definition);
    }
    for definition in authy::operations::declarations() {
        registry = registry.with_preconnection_request(definition);
    }
    let mut host = Host::new(
        store,
        snap_transport::host::Controllers::around(snap_transport::replication::Replications::new(
            replication,
        )),
        registry,
        Arc::new(snap_identity::authentication::Authentication::new(
            Arc::new(Identity::default().provider(crypto)),
            Arc::new(|| 1000),
        )),
        snap_transport::server::Config::default(),
        "authy-test".into(),
    )
    .with_inputs(|key| match key {
        "clock" => Ok(json!(1000)),
        _ => Err(snap_transport::Error::Unavailable),
    });
    host.recover().unwrap();
    host
}
fn fresh() -> Host {
    host(
        snap_store_sqlite::Sqlite::memory(&migrations()).unwrap(),
        Fake::default(),
    )
}
fn enroll(host: &mut Host, email: &str) -> (String, Account) {
    let reply = host.preconnection_reply(
        Invocation {
            id: 1,
            operation: "identity.enroll".into(),
            input: json!({"email":email,"password":"password1"}),
        },
        None,
    );
    reply.outcome.unwrap();
    let snap_transport::bearer::Change::Set(token) = reply.bearer.unwrap() else {
        panic!("missing session");
    };
    let bearer = token.expose().to_owned();
    let value = host
        .preconnection_request(
            Invocation {
                id: 2,
                operation: authy::operations::FetchAccount::NAME.into(),
                input: json!(null),
            },
            Some(bearer.clone()),
        )
        .unwrap();
    (bearer, serde_json::from_value(value).unwrap())
}

fn pump(host: &mut Host, peer: u64, client: &mut Profiles) -> Vec<Response> {
    let mut seen = Vec::new();
    for _ in 0..20 {
        while host.step() {}
        let frames = host.drain(peer).unwrap();
        if frames.is_empty() {
            return seen;
        }
        for frame in frames {
            let update = client.receive(frame.clone()).unwrap();
            assert!(update.error.is_none(), "{:?}", update.error);
            seen.push(frame);
            for command in update.send {
                host.submit(peer, command, 0).unwrap();
            }
        }
    }
    panic!("client exchange did not settle")
}
fn attach(host: &mut Host, bearer: &str, account: &Account, name: &str) -> (u64, Profiles) {
    let mut client = Profiles::new(account.profile.clone()).unwrap();
    let peer = host.open().unwrap();
    let mut command = client.connect(name).unwrap();
    let Command::Connect {
        bearer: credential, ..
    } = &mut command
    else {
        unreachable!()
    };
    *credential = bearer.into();
    host.submit(peer, command, 0).unwrap();
    pump(host, peer, &mut client);
    assert!(client.ready());
    (peer, client)
}
fn publications(frames: &[Response]) -> Vec<Publication> {
    frames
        .iter()
        .filter_map(|r| match r {
            Response::Global { kind, input } if kind == snap_transport::replication::TOPIC => {
                Some(serde_json::from_value(input.clone()).unwrap())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn account_profiles_replicate_binary_updates_to_two_clients_without_document() {
    let mut host = fresh();
    let (bearer, account) = enroll(&mut host, "Alice@Example.com");
    assert_eq!(account.email, "alice@example.com");
    let (p1, mut one) = attach(&mut host, &bearer, &account, "one");
    let (p2, mut two) = attach(&mut host, &bearer, &account, "two");
    assert_eq!(one.profile().unwrap().unwrap().first_name, "alice");
    assert_eq!(two.profile().unwrap().unwrap().revision, 1);
    let command = one.edit("  Ada  ", "  Lovelace  ").unwrap();
    assert_eq!(one.pending(), 1);
    assert_eq!(
        one.profile().unwrap().unwrap().first_name,
        "alice",
        "no optimistic overlay in this spike"
    );
    host.submit(p1, command, 0).unwrap();
    let origin = pump(&mut host, p1, &mut one);
    let subscriber = pump(&mut host, p2, &mut two);
    let expected = authy::Profile {
        id: account.profile.clone(),
        identity: account.identity.clone(),
        first_name: "Ada".into(),
        last_name: "Lovelace".into(),
        revision: 2,
    };
    assert_eq!(one.profile().unwrap(), Some(expected.clone()));
    assert_eq!(two.profile().unwrap(), Some(expected));
    assert_eq!(one.pending(), 0);
    let wire = publications(&subscriber);
    assert_eq!(wire.len(), 1);
    assert!(!wire[0].reset);
    let program =
        snap_store::Program::from_bytes(&authy::profile_catalog(), &wire[0].program).unwrap();
    assert_eq!(
        program.instructions().collect::<Vec<_>>(),
        vec![snap_store::Instruction::Update {
            table: authy::PROFILES.into(),
            key: vec![account.profile.clone().into()],
            changes: snap_store::Row::from([
                ("first_name".into(), "Ada".into()),
                ("last_name".into(), "Lovelace".into()),
                ("revision".into(), 2.into()),
            ]),
        }]
    );
    assert_eq!(publications(&origin)[0].program, wire[0].program);
    // Physical reconnect reloads a checkpoint and never reruns the edit.
    host.lost(p2, 1);
    let p2 = host.open().unwrap();
    let mut command = two.connect("two").unwrap();
    let Command::Connect {
        bearer: credential, ..
    } = &mut command
    else {
        unreachable!()
    };
    *credential = bearer;
    host.submit(p2, command, 2).unwrap();
    let reconnected = pump(&mut host, p2, &mut two);
    assert!(publications(&reconnected)[0].reset);
    assert_eq!(two.profile().unwrap().unwrap().revision, 2);
    let journals = host.transact("journal", |store_tx| {
        assert!(store_tx.find("identity.credentials", "primary", &[])?.len() == 1);
        Ok(())
    });
    journals.unwrap();
}

#[test]
fn private_tables_denials_stale_edits_and_revocation_never_export_private_programs() {
    let mut host = fresh();
    let (a_token, alice) = enroll(&mut host, "alice@example.com");
    let (b_token, bob) = enroll(&mut host, "bob@example.com");
    let (a_peer, mut a) = attach(&mut host, &a_token, &alice, "alice");
    let (b_peer, mut b) = attach(&mut host, &b_token, &bob, "bob");
    for input in [
        json!({"tables":{"identity.credentials":[["alice@example.com"]]}}),
        json!({"tables":{authy::PROFILES:[[alice.profile]]}}),
    ] {
        host.submit(
            b_peer,
            Command::Invoke(Invocation {
                id: 90,
                operation: "store.subscribe".into(),
                input,
            }),
            0,
        )
        .unwrap();
        while host.step() {}
        let denied = host.drain(b_peer).unwrap();
        assert!(denied.iter().any(|r| matches!(
            r,
            Response::Event(Event::Completed {
                outcome: Err(_),
                ..
            })
        )));
        assert!(publications(&denied).is_empty());
    }
    let input = authy::operations::EditInput {
        profile: alice.profile.clone(),
        first_name: "Hacked".into(),
        last_name: String::new(),
        revision: 1,
    };
    host.submit(
        b_peer,
        Command::Invoke(Invocation {
            id: 91,
            operation: "authy.profile.edit".into(),
            input: json!(input),
        }),
        0,
    )
    .unwrap();
    while host.step() {}
    assert!(host.drain(b_peer).unwrap().iter().any(|r| matches!(r, Response::Event(Event::Completed { outcome: Err(snap_transport::Error::Application(e)), .. }) if *e == json!(authy::operations::EditError::Denied))));

    // One durable program touches a replicated profile and a private credential.
    host.transact("mixed public/private", |tx| {
        tx.update(
            authy::PROFILES,
            &[alice.profile.clone().into()],
            snap_store::Row::from([
                ("first_name".into(), "SecretQueueName".into()),
                ("revision".into(), 2.into()),
            ]),
        )?;
        tx.update(
            "identity.credentials",
            &["alice@example.com".into()],
            snap_store::Row::from([("material".into(), "NEVER-EXPORT-THIS".into())]),
        )
    })
    .unwrap();
    // Revoke before a carrier drains the queued profile update.
    let queued = host.output(a_peer).unwrap().front().unwrap();
    let projected = publications(&[queued]);
    assert_eq!(projected.len(), 1);
    let projected =
        snap_store::Program::from_bytes(&authy::profile_catalog(), &projected[0].program).unwrap();
    assert_eq!(
        projected.instructions().count(),
        1,
        "the private credential instruction was not exported"
    );
    let resource = snap_access::Resource::new(authy::PROFILE_KIND, &alice.profile).unwrap();
    host.transact("revoke", |tx| {
        let mut changes = snap_access::ChangeSet::new(snap_access::Actor::System);
        changes.grants.push(snap_access::GrantChange {
            resource: resource.clone(),
            identity: alice.identity.clone(),
            role: None,
        });
        authy::access().change(tx, &changes).map(|_| ())
    })
    .unwrap();
    let output = host.output(a_peer).unwrap();
    let mut redacted = Vec::new();
    while let Some(frame) = output.pop_front() {
        redacted.push(frame);
    }
    assert_eq!(publications(&redacted).len(), 1);
    let program = snap_store::Program::from_bytes(
        &authy::profile_catalog(),
        &publications(&redacted)[0].program,
    )
    .unwrap();
    assert!(program.is_empty());
    for frame in redacted {
        a.receive(frame).unwrap();
    }
    assert!(a.profile().unwrap().is_none());
    assert!(a.edit("Ada", "Lovelace").is_err());
    assert!(
        pump(&mut host, b_peer, &mut b)
            .iter()
            .all(|r| !matches!(r, Response::Global { .. }))
    );
    assert_eq!(b.profile().unwrap().unwrap().first_name, "bob");
    host.transact("restore access", |tx| {
        let mut changes = snap_access::ChangeSet::new(snap_access::Actor::System);
        changes.grants.push(snap_access::GrantChange {
            resource,
            identity: alice.identity.clone(),
            role: Some(snap_access::Role::Owner),
        });
        authy::access().change(tx, &changes).map(|_| ())
    })
    .unwrap();
    let frames = pump(&mut host, a_peer, &mut a);
    assert!(publications(&frames)[0].reset);
    assert_eq!(a.profile().unwrap().unwrap().first_name, "SecretQueueName");
    let stale = authy::operations::EditInput {
        profile: alice.profile.clone(),
        first_name: "Stale".into(),
        last_name: String::new(),
        revision: 1,
    };
    host.submit(
        a_peer,
        Command::Invoke(Invocation {
            id: 92,
            operation: "authy.profile.edit".into(),
            input: json!(stale),
        }),
        0,
    )
    .unwrap();
    while host.step() {}
    let frames = host.drain(a_peer).unwrap();
    assert!(frames.iter().any(|r| matches!(r, Response::Event(Event::Completed { outcome: Err(snap_transport::Error::Application(e)), .. }) if *e == json!(authy::operations::EditError::Conflict))));
    assert!(publications(&frames).is_empty());
    let kept = host
        .transact("read", |tx| authy::Profile::read(tx, &alice.profile))
        .unwrap();
    assert_eq!(kept.revision, 2);
}

#[test]
fn failed_enrollment_and_misses_leave_no_account_profile_or_authority() {
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    let mut crypto = Fake::default();
    let create = |tx: &mut snap_store::Transaction<'_>, crypto: &mut Fake| {
        let issued =
            Identity::default().enroll(tx, crypto, "alice@example.com", "password1", 1000)?;
        authy::initialize_account(tx, &issued.principal.identity, "alice@example.com")?;
        Ok::<_, Error>(issued)
    };
    assert!(matches!(
        store.run("cold", |tx| create(tx, &mut crypto)),
        Err(Error::Miss(_))
    ));
    for table in snap_identity::TABLES
        .into_iter()
        .chain(snap_access::TABLES)
        .chain(authy::TABLES)
    {
        store.load(table).unwrap();
    }
    assert!(matches!(
        store.run("late failure", |tx| {
            create(tx, &mut crypto)?;
            Err::<(), _>(Error::Unavailable)
        }),
        Err(Error::Unavailable)
    ));
    assert!(store.programs(0, 10).unwrap().is_empty());
    for table in snap_identity::TABLES
        .into_iter()
        .chain(snap_access::TABLES)
        .chain(authy::TABLES)
    {
        assert!(
            store
                .inspect("empty", |tx| tx.find(table, "primary", &[]))
                .unwrap()
                .is_empty()
        );
    }
    let issued = store
        .run("retry", |tx| create(tx, &mut crypto))
        .unwrap()
        .value;
    let profile = authy::profile_id(&issued.principal.identity).unwrap();
    assert_eq!(
        store
            .inspect("profile", |tx| authy::Profile::read(tx, &profile))
            .unwrap()
            .first_name,
        "alice"
    );
    let before = store.programs(0, 10).unwrap().len();
    assert!(matches!(
        store.run("duplicate", |tx| create(tx, &mut crypto)),
        Err(Error::Constraint)
    ));
    assert_eq!(store.programs(0, 10).unwrap().len(), before);
    assert_eq!(
        store
            .inspect("accounts", |tx| tx.find(authy::ACCOUNTS, "primary", &[]))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn profile_update_survives_restart_and_new_client_bootstrap() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("authy.sqlite");
    snap_store_sqlite::migrate(&path, &migrations()).unwrap();
    let mut running = host(
        snap_store_sqlite::Sqlite::open(&path).unwrap(),
        Fake::default(),
    );
    let (token, account) = enroll(&mut running, "restart@example.com");
    let (peer, mut client) = attach(&mut running, &token, &account, "before");
    running
        .submit(peer, client.edit("Ada", "Lovelace").unwrap(), 0)
        .unwrap();
    pump(&mut running, peer, &mut client);
    drop(running);
    let mut reopened = snap_store_sqlite::Sqlite::open(&path).unwrap();
    let journal = reopened.programs(0, 100).unwrap();
    assert_eq!(journal.len(), 2, "only enrollment and edit write programs");
    assert!(
        journal[0]
            .program
            .instructions()
            .any(|i| i.table() == "identity.credentials")
    );
    assert_eq!(journal[1].program.instructions().count(), 1);
    let mut running = host(reopened, Fake::default());
    let (_, mut client) = attach(&mut running, &token, &account, "after");
    assert_eq!(
        client.profile().unwrap().unwrap().display_name(),
        "Ada Lovelace"
    );
}
