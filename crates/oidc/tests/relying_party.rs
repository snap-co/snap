use serde_json::json;
use snap_oidc::relying_party as rp;
use snap_store::{Error, Store};

const STATE: &str = "fixture-state-with-at-least-32-characters";
fn store() -> Store<snap_sqlite::Sqlite> {
    let mut store = snap_sqlite::Sqlite::memory(&[toml::from_str(rp::MIGRATION).unwrap()]).unwrap();
    for table in rp::TABLES {
        store.load(table).unwrap();
    }
    store
}
fn attempt() -> rp::Attempt {
    rp::Attempt {
        binding: rp::digest("browser"),
        nonce: "nonce".into(),
        verifier: "verifier".into(),
        redirect: "https://rp.test/auth/callback".into(),
        issuer: "https://issuer.test".into(),
        old_session: None,
        logout: false,
        expires: 400,
        processing: false,
    }
}
fn session() -> rp::Session {
    rp::Session {
        id: rp::digest("bearer"),
        owner: rp::owner("https://issuer.test", "person"),
        subject: "person".into(),
        issuer: "https://issuer.test".into(),
        csrf: "csrf".into(),
        nonce: "nonce".into(),
        profile: json!({"name":"Person"}),
        tokens: rp::Tokens {
            access: "access".into(),
            refresh: "refresh".into(),
            id_token: "signed".into(),
            access_expires: 200,
            auth_time: Some(100),
        },
        expires: 1000,
        refreshing: false,
        version: 1,
    }
}
fn login(store: &mut Store<snap_sqlite::Sqlite>) {
    store
        .run("start", |tx| rp::start(tx, STATE, &attempt()))
        .unwrap();
    store
        .run("consume", |tx| {
            rp::consume(tx, STATE, "browser", false, 100)
        })
        .unwrap();
    store
        .run("issue", |tx| rp::issue(tx, STATE, &session(), 100))
        .unwrap();
}

#[test]
fn browser_binding_single_use_and_atomic_issuance() {
    let mut store = store();
    store
        .run("start", |tx| rp::start(tx, STATE, &attempt()))
        .unwrap();
    assert!(matches!(
        store.run("wrong browser", |tx| rp::consume(
            tx, STATE, "attacker", false, 100
        )),
        Err(Error::NotFound)
    ));
    store
        .run("consume", |tx| {
            rp::consume(tx, STATE, "browser", false, 100)
        })
        .unwrap();
    assert!(matches!(
        store.run("replay", |tx| rp::consume(tx, STATE, "browser", false, 100)),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("rollback", |tx| {
            rp::issue(tx, STATE, &session(), 100)?;
            Err::<(), _>(Error::Constraint)
        }),
        Err(Error::Constraint)
    ));
    assert!(matches!(
        store.run("absent", |tx| rp::resolve(tx, "bearer", 100)),
        Err(Error::NotFound)
    ));
    store
        .run("issue", |tx| rp::issue(tx, STATE, &session(), 100))
        .unwrap();
    assert_eq!(
        store
            .run("read", |tx| rp::resolve(tx, "bearer", 100))
            .unwrap()
            .value
            .subject,
        "person"
    );
    assert!(matches!(
        store.run("reissue", |tx| rp::issue(tx, STATE, &session(), 100)),
        Err(Error::NotFound)
    ));
}

#[test]
fn refresh_is_single_owned_effect_and_logout_fences_late_completion() {
    let mut store = store();
    login(&mut store);
    assert!(
        store
            .run("fresh", |tx| rp::begin_refresh(tx, "bearer", 100))
            .unwrap()
            .value
            .is_none()
    );
    let previous = store
        .run("refresh", |tx| rp::begin_refresh(tx, "bearer", 180))
        .unwrap()
        .value
        .unwrap();
    assert!(matches!(
        store.run("duplicate refresh", |tx| rp::begin_refresh(
            tx, "bearer", 180
        )),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("fenced", |tx| rp::resolve(tx, "bearer", 180)),
        Err(Error::NotFound)
    ));
    let mut tokens = previous.tokens.clone();
    tokens.access_expires = 700;
    store
        .run("finish", |tx| {
            rp::finish_refresh(tx, &previous, tokens.clone(), 180)
        })
        .unwrap();
    assert!(matches!(
        store.run("late finish", |tx| rp::finish_refresh(
            tx,
            &previous,
            tokens.clone(),
            180
        )),
        Err(Error::NotFound)
    ));
    let previous = store
        .run("next refresh", |tx| rp::begin_refresh(tx, "bearer", 680))
        .unwrap()
        .value
        .unwrap();
    store.run("logout", |tx| rp::revoke(tx, "bearer")).unwrap();
    tokens.access_expires = 900;
    assert!(matches!(
        store.run("late result", |tx| rp::finish_refresh(
            tx, &previous, tokens, 680
        )),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("logged out", |tx| rp::resolve(tx, "bearer", 680)),
        Err(Error::NotFound)
    ));
}

#[test]
fn expired_attempt_and_expired_access_do_not_authorize() {
    let mut store = store();
    login(&mut store);
    assert!(matches!(
        store.run("access expired", |tx| rp::resolve(tx, "bearer", 200)),
        Err(Error::NotFound)
    ));
    store
        .run("start", |tx| rp::start(tx, STATE, &attempt()))
        .unwrap();
    assert!(matches!(
        store.run("expired", |tx| rp::consume(
            tx, STATE, "browser", false, 400
        )),
        Err(Error::NotFound)
    ));
}

#[test]
fn accepted_work_survives_refresh_but_restart_discards_uncertain_authority() {
    let mut store = store();
    login(&mut store);
    store
        .run("refresh", |tx| rp::begin_refresh(tx, "bearer", 180))
        .unwrap();
    assert_eq!(
        store
            .run("lease", |tx| rp::lease(tx, &rp::digest("bearer"), 180))
            .unwrap()
            .value
            .subject,
        "person"
    );
    store
        .run("restart recovery", |tx| rp::recover(tx, 180))
        .unwrap();
    assert!(matches!(
        store.run("retired lease", |tx| rp::lease(
            tx,
            &rp::digest("bearer"),
            180
        )),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("no retry", |tx| rp::begin_refresh(tx, "bearer", 180)),
        Err(Error::NotFound)
    ));
}

#[test]
fn verified_claims_still_require_issuer_audience_nonce_time_and_token_hash() {
    let response = json!({"access_token":"access", "refresh_token":"refresh", "id_token":"signed", "token_type":"Bearer", "expires_in":600});
    let valid = json!({"iss":"https://issuer.test", "aud":"chatty", "sub":"person", "nonce":"nonce", "exp":700, "iat":100});
    let validate = |claims: &serde_json::Value| {
        rp::validate_tokens(
            &response,
            claims,
            rp::Validation {
                issuer: "https://issuer.test",
                client: "chatty",
                nonce: Some("nonce"),
                previous: None,
                now: 100,
            },
        )
    };
    assert!(validate(&valid).is_ok());
    for (field, value) in [
        ("iss", json!("https://attacker.test")),
        ("aud", json!("another-app")),
        ("sub", json!("")),
        ("nonce", json!("other")),
        ("exp", json!(100)),
        ("iat", json!(131)),
        ("azp", json!("other")),
        ("at_hash", json!("wrong")),
    ] {
        let mut claims = valid.clone();
        claims[field] = value;
        assert!(matches!(validate(&claims), Err(Error::Invalid)), "{field}");
    }
    assert_ne!(
        rp::owner("https://issuer.test", "person"),
        rp::owner("https://other.test", "person")
    );
    assert_ne!(
        rp::owner("https://issuer.test", "person"),
        rp::owner("https://issuer.test", "Person")
    );
}
