//! Assertion claim/session policy with controlled, already-verified claims.
//! Real signature/key validation belongs to Crypto and the paired app TCP gate.
use serde_json::{Value, json};
use snap_identity::{Crypto, Identity, assertion};
use snap_store::Error;
#[allow(dead_code)]
mod support;
struct Verified(support::Fake);
impl Crypto for Verified {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        self.0.random()
    }
    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        self.0.hash_password(password)
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        self.0.verify_password(password, hash)
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        self.0.digest(secret)
    }
    fn verify_token(&self, token: &str, _: &Value) -> Result<Value, Error> {
        serde_json::from_str(token).map_err(|_| Error::Invalid)
    }
}
#[test]
fn assertion_policy_pins_purpose_issuer_audience_and_never_extends_expiry() {
    let mut store = support::store(true);
    let mut crypto = Verified(support::Fake::default());
    let claims = json!({"iss":"https://authy.test","sub":"agent-7","aud":"chatty","purpose":"snap.agent-login","iat":100,"exp":400});
    for (field, value) in [
        ("iss", json!("https://other.test")),
        ("aud", json!("other-app")),
        ("purpose", json!("id_token")),
        ("exp", json!(100)),
        ("exp", json!(401)),
        ("iat", json!(101)),
        ("sub", json!("")),
    ] {
        let mut invalid = claims.clone();
        invalid[field] = value;
        assert!(
            store
                .run("invalid-proof", |tx| assertion::acquire(
                    tx,
                    &mut crypto,
                    &invalid.to_string(),
                    "https://authy.test",
                    "chatty",
                    &json!({}),
                    100
                ))
                .is_err(),
            "{field}"
        );
    }
    assert!(
        store
            .inspect("no-side-effects", |tx| Ok(tx
                .find("identity.sessions", "primary", &[])?
                .is_empty()))
            .unwrap()
    );
    let first = store
        .run("acquire", |tx| {
            assertion::acquire(
                tx,
                &mut crypto,
                &claims.to_string(),
                "https://authy.test",
                "chatty",
                &json!({}),
                100,
            )
        })
        .unwrap()
        .value;
    let second = store
        .run("reacquire", |tx| {
            assertion::acquire(
                tx,
                &mut crypto,
                &claims.to_string(),
                "https://authy.test",
                "chatty",
                &json!({}),
                350,
            )
        })
        .unwrap()
        .value;
    assert_eq!(first.principal.identity, second.principal.identity);
    assert_ne!(first.bearer, second.bearer);
    assert_eq!(
        store
            .inspect("before-expiry", |tx| Identity::default().resolve(
                tx,
                &crypto,
                &second.bearer,
                399
            ))
            .unwrap()
            .identity,
        first.principal.identity
    );
    assert!(
        store
            .inspect("expired", |tx| Identity::default().resolve(
                tx,
                &crypto,
                &second.bearer,
                400
            ))
            .is_err()
    );
    assert!(
        store
            .run("expired-proof", |tx| assertion::acquire(
                tx,
                &mut crypto,
                &claims.to_string(),
                "https://authy.test",
                "chatty",
                &json!({}),
                400
            ))
            .is_err()
    );
}
