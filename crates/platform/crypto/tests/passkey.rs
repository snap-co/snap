use serde_json::Value;
use snap_crypto::Native as Crypto;
use snap_crypto::passkey::Native;
use snap_identity::{
    CredentialKind, Identity,
    passkey::{PasskeyProof, Passkeys, RegistrationInput},
};
use snap_store::{Error, Store};
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse, Url};

const ORIGIN: &str = "https://login.example.test";
const BINDING: &str = "caller-secret-with-at-least-32-bytes";
fn store() -> Store<snap_store_sqlite::Sqlite> {
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    Passkeys::data().prepare(&mut store).unwrap();
    store
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    [snap_identity::MIGRATION]
        .into_iter()
        .map(|s| toml::from_str(s).unwrap())
        .collect()
}

/// This is the native verifier boundary, with real signatures from an independent
/// software authenticator. Identity and Store produce credentials and sessions.
#[test]
fn registration_linking_login_replay_and_private_session_release() {
    let mut store = store();
    let identity = Identity::default();
    let passkeys = Passkeys::new(identity);
    let web = Native::new("login.example.test", ORIGIN).unwrap();
    let mut client = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let password = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut Crypto, "person@example.test", "password1", 100)
        })
        .unwrap()
        .value;
    let challenge = store
        .run("register", |tx| {
            passkeys.begin_registration(
                tx,
                &mut Crypto,
                &web,
                Some(&password.bearer),
                RegistrationInput {
                    binding: BINDING,
                    label: "My key",
                },
                101,
            )
        })
        .unwrap()
        .value;
    assert!(challenge.options.get("state").is_none());
    let options: CreationChallengeResponse = serde_json::from_value(challenge.options).unwrap();
    let response = serde_json::to_value(
        client
            .do_registration(Url::parse(ORIGIN).unwrap(), options)
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store.run("wrong binding", |tx| passkeys.finish_registration(
            tx,
            &mut Crypto,
            &web,
            Some(&password.bearer),
            PasskeyProof {
                attempt: &challenge.attempt,
                binding: "another-caller-secret-with-32-bytes",
                response: response.clone(),
            },
            102
        )),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("no session substitution", |tx| passkeys
            .finish_registration(
                tx,
                &mut Crypto,
                &web,
                None,
                PasskeyProof {
                    attempt: &challenge.attempt,
                    binding: BINDING,
                    response: response.clone(),
                },
                102
            )),
        Err(Error::NotFound)
    ));
    let linked = store
        .run("verified", |tx| {
            passkeys.finish_registration(
                tx,
                &mut Crypto,
                &web,
                Some(&password.bearer),
                PasskeyProof {
                    attempt: &challenge.attempt,
                    binding: BINDING,
                    response: response.clone(),
                },
                102,
            )
        })
        .unwrap()
        .value;
    assert_eq!(linked.principal.identity, password.principal.identity);
    assert!(matches!(
        store.run("replay", |tx| passkeys.finish_registration(
            tx,
            &mut Crypto,
            &web,
            Some(&password.bearer),
            PasskeyProof {
                attempt: &challenge.attempt,
                binding: BINDING,
                response: response.clone(),
            },
            102
        )),
        Err(Error::NotFound)
    ));
    let credentials = store
        .run("list", |tx| {
            identity.credentials(tx, &Crypto, &linked.bearer, 103)
        })
        .unwrap()
        .value;
    let key = credentials
        .iter()
        .find(|c| c.kind == CredentialKind::Passkey)
        .unwrap();
    assert!(credentials.iter().all(|c| c.removable));
    let challenge = store
        .run("authenticate", |tx| {
            passkeys.begin_authentication(tx, &mut Crypto, &web, Some(&key.locator), BINDING, 103)
        })
        .unwrap()
        .value;
    let response = assertion(&mut client, challenge.options, ORIGIN);
    assert!(matches!(
        store.run("wrong challenge", |tx| passkeys.finish_authentication(
            tx,
            &mut Crypto,
            &web,
            PasskeyProof {
                attempt: &challenge.attempt,
                binding: "another-caller-secret-with-32-bytes",
                response: response.clone(),
            },
            104
        )),
        Err(Error::NotFound)
    ));
    let login = store
        .run("login", |tx| {
            passkeys.finish_authentication(
                tx,
                &mut Crypto,
                &web,
                PasskeyProof {
                    attempt: &challenge.attempt,
                    binding: BINDING,
                    response: response.clone(),
                },
                104,
            )
        })
        .unwrap()
        .value;
    assert_eq!(login.principal.identity, password.principal.identity);
    assert_eq!(login.principal.authenticated_at, 104);
    assert!(matches!(
        store.run("assertion replay", |tx| passkeys.finish_authentication(
            tx,
            &mut Crypto,
            &web,
            PasskeyProof {
                attempt: &challenge.attempt,
                binding: BINDING,
                response: response.clone(),
            },
            104
        )),
        Err(Error::NotFound)
    ));
    store
        .run("remove password", |tx| {
            identity.remove_credential(tx, &Crypto, &login.bearer, "person@example.test", 105)
        })
        .unwrap();
    assert!(matches!(
        store.run("last credential", |tx| identity.remove_credential(
            tx,
            &Crypto,
            &login.bearer,
            &key.locator,
            105
        )),
        Err(Error::Constraint)
    ));
    // Removing a credential does not silently end existing authenticated sessions.
    store
        .run("password session still valid", |tx| {
            identity.resolve(tx, &Crypto, &password.bearer, 105)
        })
        .unwrap();
    store
        .run("release all", |tx| {
            identity.revoke_scope(tx, &Crypto, &login.bearer, "all", 105)
        })
        .unwrap();
    for bearer in [&password.bearer, &linked.bearer, &login.bearer] {
        assert!(matches!(
            store.run("released", |tx| identity.resolve(tx, &Crypto, bearer, 105)),
            Err(Error::NotFound)
        ));
    }
}
fn assertion(
    client: &mut WebauthnAuthenticator<SoftPasskey>,
    options: Value,
    origin: &str,
) -> Value {
    let options: RequestChallengeResponse = serde_json::from_value(options).unwrap();
    serde_json::to_value(
        client
            .do_authentication(Url::parse(origin).unwrap(), options)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn origin_expiry_and_latest_committed_counter_are_enforced() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("passkeys.sqlite");
    snap_store_sqlite::migrate(&path, &migrations()).unwrap();
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    Passkeys::data().prepare(&mut store).unwrap();
    let identity = Identity::default();
    let passkeys = Passkeys::new(identity);
    let web = Native::new("example.test", ORIGIN).unwrap();
    let mut client = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let challenge = store
        .run("register", |tx| {
            passkeys.begin_registration(
                tx,
                &mut Crypto,
                &web,
                None,
                RegistrationInput {
                    binding: BINDING,
                    label: "My key",
                },
                100,
            )
        })
        .unwrap()
        .value;
    let response = serde_json::to_value(
        client
            .do_registration(
                Url::parse(ORIGIN).unwrap(),
                serde_json::from_value(challenge.options).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    drop(store);
    let mut store = snap_store_sqlite::Sqlite::open(&path).unwrap();
    Passkeys::data().prepare(&mut store).unwrap();
    let issued = store
        .run("verified", |tx| {
            passkeys.finish_registration(
                tx,
                &mut Crypto,
                &web,
                None,
                PasskeyProof {
                    attempt: &challenge.attempt,
                    binding: BINDING,
                    response: response.clone(),
                },
                101,
            )
        })
        .unwrap()
        .value;
    let keys = store
        .run("keys", |tx| {
            identity.credentials(tx, &Crypto, &issued.bearer, 102)
        })
        .unwrap()
        .value;
    let locator = &keys[0].locator;
    let wrong = store
        .run("wrong origin begin", |tx| {
            passkeys.begin_authentication(tx, &mut Crypto, &web, Some(locator), BINDING, 102)
        })
        .unwrap()
        .value;
    let wrong_response = assertion(&mut client, wrong.options, "https://other.example.test");
    assert!(matches!(
        store.run("wrong origin", |tx| passkeys.finish_authentication(
            tx,
            &mut Crypto,
            &web,
            PasskeyProof {
                attempt: &wrong.attempt,
                binding: BINDING,
                response: wrong_response.clone(),
            },
            103
        )),
        Err(Error::NotFound)
    ));
    let older = store
        .run("older", |tx| {
            passkeys.begin_authentication(tx, &mut Crypto, &web, Some(locator), BINDING, 103)
        })
        .unwrap()
        .value;
    let newer = store
        .run("newer", |tx| {
            passkeys.begin_authentication(tx, &mut Crypto, &web, Some(locator), BINDING, 103)
        })
        .unwrap()
        .value;
    let old_response = assertion(&mut client, older.options, ORIGIN);
    let new_response = assertion(&mut client, newer.options, ORIGIN);
    store
        .run("newer commits first", |tx| {
            passkeys.finish_authentication(
                tx,
                &mut Crypto,
                &web,
                PasskeyProof {
                    attempt: &newer.attempt,
                    binding: BINDING,
                    response: new_response.clone(),
                },
                104,
            )
        })
        .unwrap();
    assert!(matches!(
        store.run("stale counter", |tx| passkeys.finish_authentication(
            tx,
            &mut Crypto,
            &web,
            PasskeyProof {
                attempt: &older.attempt,
                binding: BINDING,
                response: old_response.clone(),
            },
            104
        )),
        Err(Error::NotFound)
    ));
    let expired = store
        .run("expiry begin", |tx| {
            passkeys.begin_authentication(tx, &mut Crypto, &web, Some(locator), BINDING, 105)
        })
        .unwrap()
        .value;
    let response = assertion(&mut client, expired.options, ORIGIN);
    assert!(matches!(
        store.run("expiry boundary", |tx| passkeys.finish_authentication(
            tx,
            &mut Crypto,
            &web,
            PasskeyProof {
                attempt: &expired.attempt,
                binding: BINDING,
                response: response.clone(),
            },
            405
        )),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.run("stale linking proof", |tx| passkeys.begin_registration(
            tx,
            &mut Crypto,
            &web,
            Some(&issued.bearer),
            RegistrationInput {
                binding: BINDING,
                label: "Another key",
            },
            401
        )),
        Err(Error::NotFound)
    ));
    assert!(Native::new("attacker.test", ORIGIN).is_err());
    assert!(Native::new("example.test", "http://example.test").is_err());
}
