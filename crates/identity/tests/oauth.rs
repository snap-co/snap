use serde_json::json;
use snap_identity::oauth as rp;
use snap_store::{Error, Store};
#[allow(dead_code)]
mod support;
use sha2::{Digest, Sha256};
use snap_identity::{CredentialKind, Crypto as CryptoTrait, Identity};

#[derive(Default)]
struct Crypto(support::Fake);
impl CryptoTrait for Crypto {
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
        Sha256::digest(secret.as_bytes()).to_vec()
    }
}

const STATE: &str = "fixture-state-with-at-least-32-characters";
fn store() -> Store<snap_store_sqlite::Sqlite> {
    let mut migrations = snap_identity::MIGRATIONS
        .into_iter()
        .chain([rp::MIGRATION])
        .map(|s| toml::from_str::<snap_store::migration::Migration>(s).unwrap())
        .collect::<Vec<_>>();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in rp::TABLES {
        store.load(table).unwrap();
    }
    store
}
fn attempt() -> rp::Attempt {
    rp::Attempt {
        target: None,
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
fn session() -> rp::Grant {
    rp::Grant {
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
fn login(store: &mut Store<snap_store_sqlite::Sqlite>) {
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

#[test]
fn oauth_linking_uses_current_identity_and_shared_credential_session_policy() {
    let mut store = store();
    let mut crypto = Crypto::default();
    let identity = Identity::default();
    let password = store
        .run("password", |tx| {
            identity.enroll(tx, &mut crypto, "person@example.test", "password1", 100)
        })
        .unwrap()
        .value;
    let mut oauth = session();
    oauth.id = rp::digest(&"o".repeat(43));
    // A profile's matching email is metadata, not ownership proof.
    oauth.profile = json!({"email":"person@example.test"});
    let linked = store
        .run("link", |tx| {
            rp::start_link(tx, &crypto, &password.bearer, STATE, &attempt(), 101)?;
            rp::consume(tx, STATE, "browser", false, 101)?;
            rp::issue(tx, STATE, &oauth, 101)
        })
        .unwrap()
        .value;
    assert_eq!(linked.owner, password.principal.identity);
    let common = store
        .run("shared session", |tx| {
            identity.resolve(tx, &crypto, &"o".repeat(43), 102)
        })
        .unwrap()
        .value;
    assert_eq!(common.identity, password.principal.identity);
    assert_eq!(common.authenticated_at, 100);
    let labels = store
        .run("credentials", |tx| {
            identity.credentials(tx, &crypto, &password.bearer, 102)
        })
        .unwrap()
        .value;
    assert_eq!(labels.len(), 2);
    assert!(labels.iter().any(|c| c.kind == CredentialKind::OAuth));
    assert!(labels.iter().all(|c| c.removable));
    let outsider = store
        .run("outsider", |tx| {
            identity.enroll(tx, &mut crypto, "other@example.test", "password2", 102)
        })
        .unwrap()
        .value;
    assert!(matches!(
        store.run("cannot steal OAuth", |tx| {
            rp::start_link(tx, &crypto, &outsider.bearer, STATE, &attempt(), 103)?;
            rp::consume(tx, STATE, "browser", false, 103)?;
            rp::issue(tx, STATE, &oauth, 103)
        }),
        Err(Error::Constraint)
    ));
    assert!(matches!(
        store.run("cross owner deletion", |tx| identity.remove_credential(
            tx,
            &crypto,
            &outsider.bearer,
            &labels[0].locator,
            103
        )),
        Err(Error::NotFound)
    ));
    store
        .run("others", |tx| {
            identity.revoke_scope(tx, &crypto, &password.bearer, "others", 103)
        })
        .unwrap();
    assert!(matches!(
        store.run("grant cannot replace released session", |tx| rp::resolve(
            tx,
            &"o".repeat(43),
            103
        )),
        Err(Error::NotFound)
    ));
    // A subsequent callback for the linked subject still resolves to the same principal.
    oauth.id = rp::digest(&"n".repeat(43));
    let reacquired = store
        .run("oauth only", |tx| {
            rp::start(tx, STATE, &attempt())?;
            rp::consume(tx, STATE, "browser", false, 104)?;
            rp::issue(tx, STATE, &oauth, 104)
        })
        .unwrap()
        .value;
    assert_eq!(reacquired.owner, password.principal.identity);
    let oauth_locator = &labels
        .iter()
        .find(|c| c.kind == CredentialKind::OAuth)
        .unwrap()
        .locator;
    store
        .run("remove OAuth", |tx| {
            identity.remove_credential(tx, &crypto, &password.bearer, oauth_locator, 105)
        })
        .unwrap();
    assert!(matches!(
        store.run("stale management proof", |tx| identity.remove_credential(
            tx,
            &crypto,
            &password.bearer,
            &labels[0].locator,
            400
        )),
        Err(Error::NotFound)
    ));
    store
        .run("all", |tx| {
            identity.revoke_scope(tx, &crypto, &password.bearer, "all", 105)
        })
        .unwrap();
    assert!(matches!(
        store.run("released OAuth", |tx| rp::retained(tx, &oauth.id, 105)),
        Err(Error::NotFound)
    ));
}

#[test]
fn profile_email_never_links_and_unproved_freshness_cannot_manage_credentials() {
    let mut store = store();
    let mut crypto = Crypto::default();
    let identity = Identity::default();
    let password = store
        .run("password", |tx| {
            identity.enroll(tx, &mut crypto, "person@example.test", "password1", 100)
        })
        .unwrap()
        .value;
    let mut oauth = session();
    oauth.id = rp::digest(&"o".repeat(43));
    oauth.profile = json!({"email":"person@example.test", "email_verified":true});
    oauth.tokens.auth_time = None;
    let grant = store
        .run("OAuth", |tx| {
            rp::start(tx, STATE, &attempt())?;
            rp::consume(tx, STATE, "browser", false, 100)?;
            rp::issue(tx, STATE, &oauth, 100)
        })
        .unwrap()
        .value;
    assert_ne!(grant.owner, password.principal.identity);
    assert!(matches!(
        store.run("unknown freshness", |tx| identity.link_password(
            tx,
            &mut crypto,
            &"o".repeat(43),
            "another@example.test",
            "password2",
            101
        )),
        Err(Error::NotFound)
    ));
    store
        .run("valid grant", |tx| rp::resolve(tx, &"o".repeat(43), 101))
        .unwrap();
}

#[test]
fn removed_oauth_cannot_recreate_authority_for_its_old_account() {
    let mut store = store();
    let mut crypto = Crypto::default();
    let identity = Identity::default();
    let bearer = "o".repeat(43);
    let mut oauth = session();
    oauth.id = rp::digest(&bearer);
    store
        .run("OAuth", |tx| {
            rp::start(tx, STATE, &attempt())?;
            rp::consume(tx, STATE, "browser", false, 100)?;
            rp::issue(tx, STATE, &oauth, 100)
        })
        .unwrap();
    store
        .run("link password", |tx| {
            identity.link_password(
                tx,
                &mut crypto,
                &bearer,
                "recovery@example.test",
                "password1",
                101,
            )
        })
        .unwrap();
    let credentials = store
        .run("credentials", |tx| {
            identity.credentials(tx, &crypto, &bearer, 101)
        })
        .unwrap()
        .value;
    let locator = &credentials
        .iter()
        .find(|c| c.kind == CredentialKind::OAuth)
        .unwrap()
        .locator;
    store
        .run("remove OAuth", |tx| {
            identity.remove_credential(tx, &crypto, &bearer, locator, 102)
        })
        .unwrap();
    oauth.id = rp::digest(&"n".repeat(43));
    assert!(matches!(
        store.run("removed proof", |tx| {
            rp::start(tx, STATE, &attempt())?;
            rp::consume(tx, STATE, "browser", false, 103)?;
            rp::issue(tx, STATE, &oauth, 103)
        }),
        Err(Error::Constraint)
    ));
    let password = store
        .run("password still works", |tx| {
            identity.acquire(tx, &mut crypto, "recovery@example.test", "password1", 103)
        })
        .unwrap()
        .value;
    assert_eq!(password.principal.identity, oauth.owner);
}

#[test]
fn oauth_only_history_appends_identity_without_rewriting_migrations() {
    let path =
        std::env::temp_dir().join(format!("snap-oauth-upgrade-{}.sqlite", std::process::id()));
    let legacy = toml::from_str::<snap_store::migration::Migration>(rp::MIGRATION).unwrap();
    snap_store_sqlite::migrate(&path, core::slice::from_ref(&legacy)).unwrap();
    let mut old = snap_store_sqlite::Sqlite::open(&path).unwrap();
    old.load("oidc_rp.sessions").unwrap();
    let mut oauth = session();
    let bearer = "o".repeat(43);
    oauth.id = rp::digest(&bearer);
    old.run("old grant", |tx| {
        tx.insert(
            "oidc_rp.sessions",
            [
                ("id".into(), oauth.id.clone().into()),
                ("data".into(), serde_json::to_string(&oauth).unwrap().into()),
            ]
            .into_iter()
            .collect(),
        )
    })
    .unwrap();
    drop(old);
    snap_store_sqlite::migrate(
        &path,
        &[legacy, toml::from_str(rp::IDENTITY_MIGRATION).unwrap()],
    )
    .unwrap();
    let mut upgraded = snap_store_sqlite::Sqlite::open(&path).unwrap();
    rp::data().prepare(&mut upgraded).unwrap();
    assert!(matches!(
        upgraded.run("old grant is not a new session", |tx| rp::resolve(
            tx, &bearer, 100
        )),
        Err(Error::NotFound)
    ));
    upgraded.run("recover", |tx| rp::recover(tx, 100)).unwrap();
    let issued = upgraded
        .run("login again", |tx| {
            rp::start(tx, STATE, &attempt())?;
            rp::consume(tx, STATE, "browser", false, 100)?;
            rp::issue(tx, STATE, &oauth, 100)
        })
        .unwrap()
        .value;
    assert_eq!(
        issued.owner, oauth.owner,
        "existing application ownership survives the upgrade"
    );
    upgraded
        .run("link another kind", |tx| {
            Identity::default().link_password(
                tx,
                &mut Crypto::default(),
                &bearer,
                "another@example.test",
                "password1",
                101,
            )
        })
        .unwrap();
    drop(upgraded);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn legacy_owners_survive_link_first_with_or_without_a_retained_grant() {
    for retained_grant in [false, true] {
        let mut store = store();
        let mut crypto = Crypto::default();
        let identity = Identity::default();
        let mut legacy = session();
        legacy.subject = "legacy-person".into();
        legacy.owner = rp::owner(&legacy.issuer, &legacy.subject);
        let legacy_owner = legacy.owner.clone();
        if retained_grant {
            store
                .run("legacy grant", |tx| {
                    tx.insert(
                        "oidc_rp.sessions",
                        [
                            ("id".into(), legacy.id.clone().into()),
                            (
                                "data".into(),
                                serde_json::to_string(&legacy).unwrap().into(),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    )
                })
                .unwrap();
        }
        // The host supplies ownership retained by ACL/application data, even when
        // the person has no session. Recovery is not an account deletion.
        store
            .run("upgrade ownership", |tx| {
                rp::import_legacy_owners(tx, core::slice::from_ref(&legacy_owner))?;
                rp::recover(tx, 100)
            })
            .unwrap();
        let current = store
            .run("another account", |tx| {
                identity.enroll(tx, &mut crypto, "current@example.test", "password1", 100)
            })
            .unwrap()
            .value;
        legacy.id = rp::digest(&"b".repeat(43));
        assert!(matches!(
            store.run("link first", |tx| {
                rp::start_link(tx, &crypto, &current.bearer, STATE, &attempt(), 101)?;
                rp::consume(tx, STATE, "browser", false, 101)?;
                rp::issue(tx, STATE, &legacy, 101)
            }),
            Err(Error::Constraint)
        ));
        let acquired = store
            .run("legacy standalone login", |tx| {
                rp::start(tx, STATE, &attempt())?;
                rp::consume(tx, STATE, "browser", false, 102)?;
                rp::issue(tx, STATE, &legacy, 102)
            })
            .unwrap()
            .value;
        assert_eq!(acquired.owner, legacy_owner);
        assert_ne!(acquired.owner, current.principal.identity);
        store
            .run("add recovery password", |tx| {
                identity.link_password(
                    tx,
                    &mut crypto,
                    &"b".repeat(43),
                    "legacy@example.test",
                    "password2",
                    103,
                )
            })
            .unwrap();
        let credentials = store
            .run("credentials", |tx| {
                identity.credentials(tx, &crypto, &"b".repeat(43), 103)
            })
            .unwrap()
            .value;
        let locator = &credentials
            .iter()
            .find(|c| c.kind == CredentialKind::OAuth)
            .unwrap()
            .locator;
        store
            .run("remove OAuth", |tx| {
                identity.remove_credential(tx, &crypto, &"b".repeat(43), locator, 103)
            })
            .unwrap();
        // A subsequent startup must not import the owner's unchanged app records
        // again and restore a deliberately removed credential.
        store
            .run("restart ownership import", |tx| {
                rp::import_legacy_owners(tx, core::slice::from_ref(&legacy_owner))?;
                rp::recover(tx, 104)
            })
            .unwrap();
        legacy.id = rp::digest(&"c".repeat(43));
        assert!(matches!(
            store.run("removed proof after restart", |tx| {
                rp::start(tx, STATE, &attempt())?;
                rp::consume(tx, STATE, "browser", false, 104)?;
                rp::issue(tx, STATE, &legacy, 104)
            }),
            Err(Error::Constraint)
        ));
    }
}
