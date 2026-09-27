use snap_memory::{Rig, fixture::TestCrypto, store::Store};
use snap_protocol::json;

#[derive(Clone, Default)]
struct Cache(
    std::rc::Rc<
        std::cell::RefCell<std::collections::BTreeMap<snap_store::Query, snap_store::Rows>>,
    >,
);
impl snap_store::Cache for Cache {
    fn get(&self, query: &snap_store::Query) -> Option<snap_store::Rows> {
        self.0.borrow().get(query).cloned()
    }
    fn put(&self, query: snap_store::Query, rows: snap_store::Rows) {
        self.0.borrow_mut().insert(query, rows);
    }
}

fn enrollment_after_miss(cache: impl snap_store::Cache) {
    let mut rig = Rig::new(authy::server(
        Store::new(&authy::schemas()).unwrap(),
        TestCrypto::default(),
        cache,
    ));
    let client = rig.client(authy::client());
    rig.run_until_stalled();
    let credentials =
        json!({"kind":"user", "email":"later@example.test", "password":"original password"});
    let error = rig
        .run(client.command("identity.password.acquire", Some(credentials.clone())))
        .unwrap_err();
    assert_eq!(json!(error)["failure"]["_tag"], "InvalidCredentialError");
    rig.run(client.command("account.create", Some(credentials.clone())))
        .unwrap();
    rig.run(client.command("identity.release", Some(json!({"scope":"current"}))))
        .unwrap();
    rig.run(client.command("identity.password.acquire", Some(credentials)))
        .unwrap();
    rig.run_until_stalled();
    assert_eq!(client.snapshot().phase, "identified");
}

#[test]
fn credential_absence_before_enrollment_does_not_prevent_login() {
    enrollment_after_miss(snap_store::NoCache);
    enrollment_after_miss(Cache::default());
}

#[test]
fn duplicate_enrollment_wrong_password_and_revoke_all_preserve_authority() {
    let mut rig = Rig::new(authy::server(
        Store::new(&authy::schemas()).unwrap(),
        TestCrypto::default(),
        snap_store::NoCache,
    ));
    let first = rig.client(authy::client());
    let second = rig.client(authy::client());
    rig.run_until_stalled();
    let credentials =
        json!({"email":"person@example.test", "password":"original password", "kind":"user"});
    rig.run(first.command("account.create", Some(credentials.clone())))
        .unwrap();
    let duplicate = rig
        .run(second.command(
            "account.create",
            Some(json!({"email":" PERSON@example.test ","password":"different password"})),
        ))
        .unwrap_err();
    assert_eq!(json!(duplicate)["failure"]["_tag"], "EnrollFailedError");
    let wrong = rig
        .run(second.command(
            "identity.password.acquire",
            Some(json!({"kind":"user","email":"person@example.test","password":"wrong password"})),
        ))
        .unwrap_err();
    assert_eq!(json!(wrong)["failure"]["_tag"], "InvalidCredentialError");
    assert_eq!(second.snapshot().phase, "anonymous");
    rig.run(second.command("identity.password.acquire", Some(credentials)))
        .unwrap();
    rig.run_until_stalled();
    assert_eq!(first.snapshot().identity_id, second.snapshot().identity_id);
    rig.run(first.command("identity.release", Some(json!({"scope":"all"}))))
        .unwrap();
    rig.run_until_stalled();
    assert_eq!(first.snapshot().phase, "anonymous");
    assert_eq!(second.snapshot().phase, "anonymous");
    first.close();
    assert!(rig.run(first.command("refresh", None)).is_err());
}

#[test]
fn account_session_journey_through_the_real_sdk_without_io() {
    let store = Store::new(&authy::schemas()).unwrap();
    let server = authy::server(store, TestCrypto::default(), snap_store::NoCache);
    let mut rig = Rig::new(server);
    let first = rig.client(authy::client());
    let second = rig.client(authy::client());
    rig.run_until_stalled();
    rig.run(first.command(
        "account.create",
        Some(json!({"email":" Person@Example.test ","password":"password sessions"})),
    ))
    .unwrap();
    rig.run_until_stalled();
    let identity = first.snapshot().identity_id.unwrap();
    rig.run(second.command(
        "identity.password.acquire",
        Some(json!({"kind":"user","email":"person@example.test","password":"password sessions"})),
    ))
    .unwrap();
    rig.run_until_stalled();
    rig.run(second.command("identity.release", Some(json!({"scope":"others"}))))
        .unwrap();
    rig.run_until_stalled();

    let remaining = second.snapshot();
    assert_eq!(first.snapshot().phase, "anonymous");
    assert_eq!(remaining.identity_id.as_deref(), Some(identity.as_str()));
    assert_eq!(remaining.connection, "connected");
    assert_eq!(remaining.credentials[0]["label"], "person@example.test");
    assert_eq!(remaining.credentials[0]["removable"], false);
    assert_eq!(remaining.sessions.len(), 1);
    assert_eq!(remaining.sessions[0]["current"], true);
}

#[test]
fn invalid_enrollment_is_rejected_before_acceptance_and_does_not_claim_email() {
    let server = authy::server(
        Store::new(&authy::schemas()).unwrap(),
        TestCrypto::default(),
        snap_store::NoCache,
    );
    let mut rig = Rig::new(server);
    let client = rig.client(authy::client());
    rig.run_until_stalled();
    assert!(
        rig.run(client.command(
            "account.create",
            Some(json!({"email":"person@example.test","password":"short"}))
        ))
        .is_err()
    );
    let events: Vec<_> = rig
        .trace()
        .into_iter()
        .filter(|e| e.operation == "account.create")
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].phase, snap_memory::Phase::Rejected);
    rig.run(client.command(
        "account.create",
        Some(json!({"email":"person@example.test","password":"password sessions"})),
    ))
    .unwrap();
    rig.run_until_stalled();
    assert_eq!(client.snapshot().phase, "identified");
}

#[test]
fn session_expiry_uses_virtual_time() {
    let server = authy::server(
        Store::new(&authy::schemas()).unwrap(),
        TestCrypto::default(),
        snap_store::NoCache,
    );
    let mut rig = Rig::new(server);
    let client = rig.client(authy::client());
    rig.run_until_stalled();
    rig.run(client.command(
        "account.create",
        Some(json!({"email":"expiry@example.test","password":"password sessions"})),
    ))
    .unwrap();
    rig.run_until_stalled();
    rig.clock
        .advance(snap_runtime::passport::SESSION_SECONDS * 1000);
    rig.run_until_stalled();
    assert_eq!(client.snapshot().phase, "anonymous");
}

#[test]
fn account_profile_is_created_with_enrollment_and_snapshots_are_isolated() {
    let store = Store::new(&authy::schemas()).unwrap();
    let crypto = TestCrypto::default();
    let passport = authy::server(store.clone(), crypto.clone(), snap_store::NoCache);
    let accounts = authy::account::Accounts {
        store: store.clone(),
        passport: passport.clone(),
    };
    let mut rig = Rig::new(passport);
    let call = rig.submit(
        snap_protocol::Invocation {
            operation_id: "enroll".into(),
            key: "account.create".into(),
            payload: Some(json!({"email":"profile@example.test","password":"password sessions"})),
            traceparent: None,
        },
        snap_runtime::passport::Context::default(),
    );
    let reply = rig.complete(call);
    reply.outcome.unwrap();
    let token = reply.token.flatten().unwrap();
    let actor = http_ok(rig.run(accounts.resolve(&token, 0))).unwrap();
    let profile = http_ok(rig.run(accounts.profile(actor.clone())));
    let baseline = store.snapshot();
    http_ok(rig.run(accounts.update(
        actor,
        json!({"name":"Changed","bio":"","revision":profile["revision"]}),
        1,
    )));

    let restored = Store::from_snapshot(baseline);
    let restored_accounts = authy::account::Accounts {
        store: restored.clone(),
        passport: authy::server(restored, crypto, snap_store::NoCache),
    };
    let actor = http_ok(rig.run(restored_accounts.resolve(&token, 0))).unwrap();
    let restored_profile = http_ok(rig.run(restored_accounts.profile(actor)));
    assert_eq!(restored_profile["name"], "profile");
    assert_eq!(restored_profile["email"], "profile@example.test");
    assert_eq!(restored_profile["revision"], 1);
}

fn http_ok<T>(result: Result<T, snap_http::Response>) -> T {
    result.unwrap_or_else(|response| panic!("Unexpected HTTP failure: {}", response.status))
}

#[test]
fn closing_before_delivery_cancels_the_queued_command() {
    let server = authy::server(
        Store::new(&authy::schemas()).unwrap(),
        TestCrypto::default(),
        snap_store::NoCache,
    );
    let mut rig = Rig::new(server);
    let client = rig.client(authy::client());
    rig.run_until_stalled();
    let command = client.command(
        "account.create",
        Some(json!({"email":"closed@example.test","password":"password sessions"})),
    );
    client.close();
    assert!(rig.run(command).is_err());
    rig.run_until_stalled();
    assert_eq!(client.snapshot().phase, "closed");
    assert!(!rig.trace().iter().any(|e| e.operation == "account.create"));
}
