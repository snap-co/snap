use sha2::{Digest, Sha256};
use snap_identity::Crypto as _;
use snap_oidc::{
    Authority, AuthorizeOutcome, AuthorizeRequest, BrowserSession, Claims, Client, ClientAuth,
    Config, ConsentOutcome, ExchangeOutcome, Host, IdHint, LogoutConfirm, LogoutConfirmOutcome,
    LogoutOutcome, LogoutRequest, RefreshOutcome, ResumeOutcome, RevokeOutcome, UserinfoOutcome,
};
use snap_store::{Error, Store};

const ISSUER: &str = "http://authy.test";
const CALLBACK: &str = "http://127.0.0.1:3850/auth/callback";
const LOGGED_OUT: &str = "http://127.0.0.1:3850/auth/logged-out";
// RFC 7636 appendix B vector: verifier <-> challenge.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const SECRET: &str = "oidc-fixture-only-secret-32-characters";
const NOW: i64 = 1_700_000_000;

struct TestHost {
    counter: u64,
}

impl TestHost {
    fn new() -> Self {
        Self { counter: 0 }
    }
}

impl Host for TestHost {
    fn random(&mut self) -> Result<String, Error> {
        self.counter += 1;
        Ok(format!(
            "test-material-{:032}-{counter}",
            self.counter,
            counter = self.counter * 7
        ))
    }

    fn digest(&self, secret: &str) -> Vec<u8> {
        Sha256::digest(secret.as_bytes()).to_vec()
    }

    fn sign(&self, claims: &serde_json::Value) -> Result<String, Error> {
        serde_json::to_string(claims).map_err(|_| Error::Invalid)
    }
}

struct IdCrypto {
    counter: u64,
}

impl IdCrypto {
    fn new() -> Self {
        Self { counter: 0 }
    }
}

impl snap_identity::Crypto for IdCrypto {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        self.counter += 1;
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&self.counter.to_be_bytes());
        bytes[8..16].copy_from_slice(&(!self.counter).to_be_bytes());
        Ok(bytes)
    }

    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        Ok(format!("fake:{password}"))
    }

    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        Ok(hash == format!("fake:{password}"))
    }

    fn digest(&self, secret: &str) -> Vec<u8> {
        Sha256::digest(secret.as_bytes()).to_vec()
    }
}

struct TestAuthority {
    identity: snap_identity::Identity,
}

impl Authority for TestAuthority {
    fn subject(
        &self,
        tx: &mut snap_store::Transaction<'_>,
        session: &[u8],
        now: i64,
    ) -> Result<String, Error> {
        Ok(self.identity.resolve_digest(tx, session, now)?.identity)
    }
}

fn migrations() -> Vec<snap_store::migration::Migration> {
    [snap_identity::MIGRATION, snap_oidc::MIGRATION]
        .into_iter()
        .map(|text| toml::from_str(text).unwrap())
        .collect()
}

fn store() -> Store<snap_sqlite::Sqlite> {
    let mut store = snap_sqlite::Sqlite::memory(&migrations()).unwrap();
    for table in snap_identity::TABLES.iter().chain(snap_oidc::TABLES.iter()) {
        store.load(table).unwrap();
    }
    store
}

fn cold_store() -> Store<snap_sqlite::Sqlite> {
    snap_sqlite::Sqlite::memory(&migrations()).unwrap()
}

fn config() -> Config {
    Config {
        issuer: ISSUER.into(),
        clients: vec![
            Client::public("chatty", "Chatty", CALLBACK, LOGGED_OUT).unwrap(),
            Client::confidential(
                "chatty-conf",
                "Chatty Confidential",
                CALLBACK,
                LOGGED_OUT,
                Sha256::digest(SECRET.as_bytes()).to_vec(),
            )
            .unwrap(),
        ],
    }
}

fn claims() -> Claims {
    Claims {
        name: "OIDC Person".into(),
        email: "oidc@example.test".into(),
        email_verified: false,
        updated_at: NOW,
    }
}

fn public_auth() -> ClientAuth<'static> {
    ClientAuth {
        client_id: "chatty",
        secret: None,
        body_had_secret: false,
    }
}

fn enroll(
    store: &mut Store<snap_sqlite::Sqlite>,
    crypto: &mut IdCrypto,
) -> (String, BrowserSession) {
    enroll_as(store, crypto, "oidc@example.test")
}

fn enroll_as(
    store: &mut Store<snap_sqlite::Sqlite>,
    crypto: &mut IdCrypto,
    email: &str,
) -> (String, BrowserSession) {
    let issued = store
        .run("enroll", |tx| {
            snap_identity::Identity::default().enroll(tx, crypto, email, "password for oidc", NOW)
        })
        .unwrap()
        .value;
    let session = BrowserSession {
        subject: issued.session.identity.clone(),
        session: crypto.digest(&issued.bearer),
        auth_time: issued.session.expires - 30 * 24 * 60 * 60,
    };
    (issued.bearer, session)
}

fn authorize_request<'a>() -> AuthorizeRequest<'a> {
    AuthorizeRequest {
        client_id: "chatty",
        redirect_uri: CALLBACK,
        response_type: "code",
        scope: "openid profile email",
        state: "test-state",
        nonce: "test-nonce",
        code_challenge: CHALLENGE,
        code_challenge_method: "S256",
        prompt: "",
        max_age: None,
        hint_subject: None,
        response_mode: None,
        has_request: false,
        has_request_uri: false,
        has_registration: false,
    }
}

fn authorize(
    store: &mut Store<snap_sqlite::Sqlite>,
    host: &mut TestHost,
    authority: &TestAuthority,
    config: &Config,
    session: &BrowserSession,
    now: i64,
) -> String {
    let req = authorize_request();
    let outcome = store
        .run("authorize", |tx| {
            snap_oidc::authorize(tx, host, authority, config, &req, Some(session), now)
        })
        .unwrap()
        .value;
    let AuthorizeOutcome::ShowConsent { handle } = outcome else {
        panic!("expected consent continuation");
    };
    handle
}

fn consent(
    store: &mut Store<snap_sqlite::Sqlite>,
    host: &mut TestHost,
    authority: &TestAuthority,
    config: &Config,
    session: &BrowserSession,
    handle: &str,
    now: i64,
) -> String {
    let outcome = store
        .run("consent", |tx| {
            snap_oidc::consent(
                tx,
                host,
                authority,
                config,
                &snap_oidc::ConsentRequest {
                    handle,
                    decision: "allow",
                    origin: ISSUER,
                },
                Some(session),
                now,
            )
        })
        .unwrap()
        .value;
    let ConsentOutcome::Redirect { uri } = outcome else {
        panic!("expected code redirect");
    };
    let code = uri
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    assert!(uri.contains("state=test-state"));
    assert!(uri.contains("iss=http://authy.test"));
    assert!(uri.starts_with(CALLBACK));
    code.to_string()
}

fn exchange(
    store: &mut Store<snap_sqlite::Sqlite>,
    host: &mut TestHost,
    authority: &TestAuthority,
    config: &Config,
    code: &str,
    now: i64,
) -> snap_oidc::Tokens {
    let outcome = store
        .run("exchange", |tx| {
            snap_oidc::exchange_code(
                tx,
                host,
                authority,
                config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                now,
            )
        })
        .unwrap()
        .value;
    let ExchangeOutcome::Issued(tokens) = outcome else {
        panic!("expected issuance");
    };
    tokens
}

fn full_grant(
    store: &mut Store<snap_sqlite::Sqlite>,
    host: &mut TestHost,
    authority: &TestAuthority,
    config: &Config,
    crypto: &mut IdCrypto,
    now: i64,
) -> (BrowserSession, snap_oidc::Tokens) {
    let (_bearer, session) = enroll(store, crypto);
    let handle = authorize(store, host, authority, config, &session, now);
    let code = consent(store, host, authority, config, &session, &handle, now);
    let tokens = exchange(store, host, authority, config, &code, now);
    (session, tokens)
}

#[test]
fn migration_applies_and_tables_load() {
    let parsed: snap_store::migration::Migration = toml::from_str(snap_oidc::MIGRATION).unwrap();
    assert_eq!(parsed.id, "0001_oidc");
    let _ = store();
    assert_eq!(snap_oidc::TABLES.len(), 3);
}

#[test]
fn unknown_client_and_bad_redirect_never_redirect() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let outcome = store
        .run("authorize", |tx| {
            snap_oidc::authorize(
                tx,
                &mut host,
                &authority,
                &config,
                &AuthorizeRequest {
                    client_id: "unknown",
                    ..authorize_request()
                },
                None,
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, AuthorizeOutcome::DirectError { .. }));
    let outcome = store
        .run("authorize", |tx| {
            snap_oidc::authorize(
                tx,
                &mut host,
                &authority,
                &config,
                &AuthorizeRequest {
                    redirect_uri: "https://attacker.example/",
                    ..authorize_request()
                },
                None,
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, AuthorizeOutcome::DirectError { .. }));
}

#[test]
fn prompt_none_without_session_is_login_required_redirect() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let outcome = store
        .run("authorize", |tx| {
            snap_oidc::authorize(
                tx,
                &mut host,
                &authority,
                &config,
                &AuthorizeRequest {
                    prompt: "none",
                    ..authorize_request()
                },
                None,
                NOW,
            )
        })
        .unwrap()
        .value;
    let AuthorizeOutcome::RedirectError {
        error, redirect, ..
    } = outcome
    else {
        panic!("expected redirect error");
    };
    assert_eq!(error, "login_required");
    assert_eq!(redirect, CALLBACK);
}

#[test]
fn invalid_scope_pkce_and_prompt_are_redirect_errors() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    for req in [
        AuthorizeRequest {
            scope: "profile email",
            ..authorize_request()
        },
        AuthorizeRequest {
            scope: "openid admin",
            ..authorize_request()
        },
        AuthorizeRequest {
            code_challenge: "short",
            ..authorize_request()
        },
        AuthorizeRequest {
            code_challenge_method: "plain",
            ..authorize_request()
        },
        AuthorizeRequest {
            response_type: "token",
            ..authorize_request()
        },
        AuthorizeRequest {
            prompt: "none consent",
            ..authorize_request()
        },
        AuthorizeRequest {
            max_age: Some("soon"),
            ..authorize_request()
        },
        AuthorizeRequest {
            has_request: true,
            ..authorize_request()
        },
    ] {
        let outcome = store
            .run("authorize", |tx| {
                snap_oidc::authorize(tx, &mut host, &authority, &config, &req, None, NOW)
            })
            .unwrap()
            .value;
        assert!(matches!(outcome, AuthorizeOutcome::RedirectError { .. }));
    }
}

#[test]
fn stale_max_age_requires_fresh_login() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let outcome = store
        .run("authorize", |tx| {
            snap_oidc::authorize(
                tx,
                &mut host,
                &authority,
                &config,
                &AuthorizeRequest {
                    max_age: Some("0"),
                    ..authorize_request()
                },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    let AuthorizeOutcome::RequireLogin { handle, .. } = outcome else {
        panic!("expected login continuation");
    };
    // Same session cannot resume its own stale continuation.
    let outcome = store
        .run("resume", |tx| {
            snap_oidc::resume(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ResumeRequest { handle: &handle },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ResumeOutcome::LoginRequired));
    // A fresh login resumes into consent.
    let issued = store
        .run("login", |tx| {
            snap_identity::Identity::default().login(
                tx,
                &mut crypto,
                "oidc@example.test",
                "password for oidc",
                NOW + 10,
            )
        })
        .unwrap()
        .value;
    let fresh = BrowserSession {
        subject: issued.session.identity.clone(),
        session: crypto.digest(&issued.bearer),
        auth_time: issued.session.expires - 30 * 24 * 60 * 60,
    };
    let outcome = store
        .run("resume", |tx| {
            snap_oidc::resume(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ResumeRequest { handle: &handle },
                Some(&fresh),
                NOW + 10,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ResumeOutcome::ShowConsent { .. }));
}

#[test]
fn consent_continuation_is_single_use() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let _ = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let outcome = store
        .run("consent-reuse", |tx| {
            snap_oidc::consent(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ConsentRequest {
                    handle: &handle,
                    decision: "allow",
                    origin: ISSUER,
                },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ConsentOutcome::InvalidGrant));
}

#[test]
fn consent_denies_with_access_denied_redirect() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let outcome = store
        .run("deny", |tx| {
            snap_oidc::consent(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ConsentRequest {
                    handle: &handle,
                    decision: "deny",
                    origin: ISSUER,
                },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ConsentOutcome::RedirectError { .. }));
}

#[test]
fn confidential_client_requires_basic_secret() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let req = AuthorizeRequest {
        client_id: "chatty-conf",
        ..authorize_request()
    };
    let outcome = store
        .run("authorize", |tx| {
            snap_oidc::authorize(
                tx,
                &mut host,
                &authority,
                &config,
                &req,
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    let AuthorizeOutcome::ShowConsent { handle } = outcome else {
        panic!("expected consent");
    };
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    // Authorize/consent bind the confidential client id; token call Sites:
    // no secret -> 401, wrong secret -> 401, body secret flag -> 401.
    for auth in [
        ClientAuth {
            client_id: "chatty-conf",
            secret: None,
            body_had_secret: false,
        },
        ClientAuth {
            client_id: "chatty-conf",
            secret: Some("wrong-secret"),
            body_had_secret: false,
        },
        ClientAuth {
            client_id: "chatty-conf",
            secret: Some(SECRET),
            body_had_secret: true,
        },
        ClientAuth {
            client_id: "chatty",
            secret: Some("unexpected"),
            body_had_secret: false,
        },
    ] {
        let outcome = store
            .run("exchange-auth", |tx| {
                snap_oidc::exchange_code(
                    tx,
                    &mut host,
                    &authority,
                    &config,
                    &snap_oidc::ExchangeRequest {
                        auth: &auth,
                        code: &code,
                        redirect_uri: CALLBACK,
                        verifier: VERIFIER,
                        claims: &claims(),
                    },
                    NOW,
                )
            })
            .unwrap()
            .value;
        assert!(matches!(outcome, ExchangeOutcome::InvalidClient));
    }
    // Correct secret succeeds.
    let outcome = store
        .run("exchange-ok", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &ClientAuth {
                        client_id: "chatty-conf",
                        secret: Some(SECRET),
                        body_had_secret: false,
                    },
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ExchangeOutcome::Issued(_)));
}

#[test]
fn wrong_verifier_redirect_mismatch_fail_without_revocation() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let outcome = store
        .run("bad-verifier", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: &"x".repeat(43),
                    claims: &claims(),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ExchangeOutcome::InvalidGrant));
    let outcome = store
        .run("bad-redirect", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: "https://attacker.example/",
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ExchangeOutcome::InvalidGrant));
    // The code survives failed validations and still redeems.
    let tokens = exchange(&mut store, &mut host, &authority, &config, &code, NOW);
    assert_eq!(tokens.expires_in, 600);
    assert_eq!(tokens.scope, "openid profile email");
}

#[test]
fn id_token_preserves_nonce_and_auth_time_with_at_hash() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let id_claims: serde_json::Value = serde_json::from_str(&tokens.id_token).unwrap();
    assert_eq!(id_claims["iss"], ISSUER);
    assert_eq!(id_claims["aud"], "chatty");
    assert_eq!(id_claims["nonce"], "test-nonce");
    assert_eq!(
        id_claims["exp"].as_i64().unwrap() - id_claims["iat"].as_i64().unwrap(),
        600
    );
    assert!(id_claims["sub"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(id_claims["sid"].as_str().is_some_and(|s| !s.is_empty()));
    let expected = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        URL_SAFE_NO_PAD.encode(&Sha256::digest(tokens.access.as_bytes())[..16])
    };
    assert_eq!(id_claims["at_hash"], expected);
    let auth_time = id_claims["auth_time"].as_i64().unwrap();
    let outcome = store
        .run("refresh", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: None,
                    claims: &claims(),
                },
                NOW + 60,
            )
        })
        .unwrap()
        .value;
    let RefreshOutcome::Issued(rotated) = outcome else {
        panic!("expected rotation");
    };
    let rotated_claims: serde_json::Value = serde_json::from_str(&rotated.id_token).unwrap();
    assert_eq!(rotated_claims["auth_time"], auth_time);
    assert_eq!(rotated_claims["nonce"], "test-nonce");
    assert_ne!(rotated.refresh, tokens.refresh);
}

#[test]
fn code_replay_revokes_family_and_commits() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let tokens = exchange(&mut store, &mut host, &authority, &config, &code, NOW);
    // Replay reports invalid_grant but commits the family revocation.
    let outcome = store
        .run("replay", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                NOW + 1,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ExchangeOutcome::InvalidGrant));
    let outcome = store
        .run("userinfo-after-replay", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &claims(),
                },
                NOW + 1,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, UserinfoOutcome::InvalidToken));
}

#[test]
fn refresh_reuse_revokes_rotated_family() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let outcome = store
        .run("rotate", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: None,
                    claims: &claims(),
                },
                NOW + 10,
            )
        })
        .unwrap()
        .value;
    let RefreshOutcome::Issued(rotated) = outcome else {
        panic!("expected rotation");
    };
    // Reusing the consumed predecessor revokes the family, including the
    // just-rotated successors, and still commits.
    let outcome = store
        .run("reuse", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: None,
                    claims: &claims(),
                },
                NOW + 11,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RefreshOutcome::InvalidGrant));
    for token in [&rotated.access, &rotated.refresh] {
        let is_access = token == &rotated.access;
        if is_access {
            let outcome = store
                .run("check", |tx| {
                    snap_oidc::userinfo(
                        tx,
                        &host,
                        &authority,
                        &snap_oidc::UserinfoRequest {
                            token,
                            claims: &claims(),
                        },
                        NOW + 12,
                    )
                })
                .unwrap()
                .value;
            assert!(matches!(outcome, UserinfoOutcome::InvalidToken));
        } else {
            let outcome = store
                .run("check", |tx| {
                    snap_oidc::refresh(
                        tx,
                        &mut host,
                        &authority,
                        &config,
                        &snap_oidc::RefreshRequest {
                            auth: &public_auth(),
                            token,
                            scope: None,
                            claims: &claims(),
                        },
                        NOW + 12,
                    )
                })
                .unwrap()
                .value;
            assert!(matches!(outcome, RefreshOutcome::InvalidGrant));
        }
    }
}

#[test]
fn refresh_scope_must_match_grant() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let outcome = store
        .run("scope", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: Some("openid"),
                    claims: &claims(),
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RefreshOutcome::InvalidScope));
}

#[test]
fn session_revocation_invalidates_grant_family() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let tokens = exchange(&mut store, &mut host, &authority, &config, &code, NOW);
    store
        .run("revoke-session", |tx| {
            snap_identity::Identity::default().revoke(tx, &crypto, &bearer, NOW + 20)
        })
        .unwrap();
    let outcome = store
        .run("refresh-dead", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: None,
                    claims: &claims(),
                },
                NOW + 21,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RefreshOutcome::InvalidGrant));
    let outcome = store
        .run("userinfo-dead", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &claims(),
                },
                NOW + 21,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, UserinfoOutcome::InvalidToken));
}

#[test]
fn session_expiry_invalidates_grant_family() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let late = NOW + 30 * 24 * 60 * 60 + 1;
    let outcome = store
        .run("refresh-expired", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: None,
                    claims: &claims(),
                },
                late,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RefreshOutcome::InvalidGrant));
}

#[test]
fn time_limits_are_enforced() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    // Code expires after 60 seconds.
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let outcome = store
        .run("late-code", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                NOW + 60,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ExchangeOutcome::InvalidGrant));
    // Continuations expire after five minutes.
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let outcome = store
        .run("late-consent", |tx| {
            snap_oidc::consent(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ConsentRequest {
                    handle: &handle,
                    decision: "allow",
                    origin: ISSUER,
                },
                Some(&session),
                NOW + 300,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, ConsentOutcome::InvalidGrant));
    // Access tokens expire after ten minutes; families after 30 days.
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let tokens = exchange(&mut store, &mut host, &authority, &config, &code, NOW);
    let outcome = store
        .run("late-info", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &claims(),
                },
                NOW + 600,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, UserinfoOutcome::InvalidToken));
    let outcome = store
        .run("late-family", |tx| {
            snap_oidc::refresh(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::RefreshRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                    scope: None,
                    claims: &claims(),
                },
                NOW + 30 * 24 * 60 * 60,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RefreshOutcome::InvalidGrant));
}

#[test]
fn userinfo_returns_scoped_claims() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let outcome = store
        .run("userinfo", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &claims(),
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    let UserinfoOutcome::Claims(value) = outcome else {
        panic!("expected claims");
    };
    assert_eq!(value["name"], "OIDC Person");
    assert_eq!(value["email"], "oidc@example.test");
    assert_eq!(value["email_verified"], false);
    let outcome = store
        .run("userinfo-bad", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: "not-a-token",
                    claims: &claims(),
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, UserinfoOutcome::InvalidToken));
}

#[test]
fn revoke_ends_family_but_stays_quiet() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let outcome = store
        .run("revoke-unknown", |tx| {
            snap_oidc::revoke(
                tx,
                &host,
                &config,
                &snap_oidc::RevokeRequest {
                    auth: &public_auth(),
                    token: "unknown-token",
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RevokeOutcome::Revoked));
    let outcome = store
        .run("revoke", |tx| {
            snap_oidc::revoke(
                tx,
                &host,
                &config,
                &snap_oidc::RevokeRequest {
                    auth: &public_auth(),
                    token: &tokens.refresh,
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, RevokeOutcome::Revoked));
    let outcome = store
        .run("info-after-revoke", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &claims(),
                },
                NOW + 6,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, UserinfoOutcome::InvalidToken));
}

#[test]
fn failed_transactions_commit_nothing() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let code_copy = code.clone();
    let result = store.run("aborted-exchange", |tx| {
        let outcome = snap_oidc::exchange_code(
            tx,
            &mut host,
            &authority,
            &config,
            &snap_oidc::ExchangeRequest {
                auth: &public_auth(),
                code: &code_copy,
                redirect_uri: CALLBACK,
                verifier: VERIFIER,
                claims: &claims(),
            },
            NOW,
        )?;
        assert!(matches!(outcome, ExchangeOutcome::Issued(_)));
        Err::<(), _>(Error::Unavailable)
    });
    assert!(matches!(result, Err(Error::Unavailable)));
    // The code was never consumed, so a retry issues cleanly.
    let tokens = exchange(&mut store, &mut host, &authority, &config, &code, NOW);
    assert!(!tokens.access.is_empty());
}

#[test]
fn logout_validates_targets_and_confirms() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    // Unregistered logout targets never create continuations.
    let outcome = store
        .run("logout-bad", |tx| {
            snap_oidc::logout(
                tx,
                &mut host,
                &authority,
                &config,
                &LogoutRequest {
                    client_id: Some("chatty"),
                    post_logout_redirect_uri: Some("https://attacker.example/"),
                    hint: None,
                    state: "s",
                    session: Some(&session),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, LogoutOutcome::DirectError { .. }));
    let outcome = store
        .run("logout-hint-mismatch", |tx| {
            snap_oidc::logout(
                tx,
                &mut host,
                &authority,
                &config,
                &LogoutRequest {
                    client_id: Some("chatty"),
                    post_logout_redirect_uri: Some(LOGGED_OUT),
                    hint: Some(&IdHint {
                        aud: "other".into(),
                    }),
                    state: "s",
                    session: Some(&session),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, LogoutOutcome::DirectError { .. }));
    // No session redirects at once.
    let outcome = store
        .run("logout-anon", |tx| {
            snap_oidc::logout(
                tx,
                &mut host,
                &authority,
                &config,
                &LogoutRequest {
                    client_id: Some("chatty"),
                    post_logout_redirect_uri: Some(LOGGED_OUT),
                    hint: None,
                    state: "logout-state",
                    session: None,
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    let LogoutOutcome::Redirect { uri } = outcome else {
        panic!("expected redirect");
    };
    assert!(uri.contains("state=logout-state"));
    // Confirmation binds the exact session and origin.
    let outcome = store
        .run("logout", |tx| {
            snap_oidc::logout(
                tx,
                &mut host,
                &authority,
                &config,
                &LogoutRequest {
                    client_id: Some("chatty"),
                    post_logout_redirect_uri: Some(LOGGED_OUT),
                    hint: Some(&IdHint {
                        aud: "chatty".into(),
                    }),
                    state: "logout-state",
                    session: Some(&session),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    let LogoutOutcome::ShowConfirm { handle } = outcome else {
        panic!("expected confirmation");
    };
    let outcome = store
        .run("logout-origin", |tx| {
            snap_oidc::logout_confirm(
                tx,
                &host,
                &config,
                &LogoutConfirm {
                    handle: &handle,
                    origin: "https://attacker.example",
                    session: Some(&session),
                },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, LogoutConfirmOutcome::Forbidden));
    let issued = store
        .run("login2", |tx| {
            snap_identity::Identity::default().login(
                tx,
                &mut crypto,
                "oidc@example.test",
                "password for oidc",
                NOW + 1,
            )
        })
        .unwrap()
        .value;
    let other = BrowserSession {
        subject: issued.session.identity.clone(),
        session: crypto.digest(&issued.bearer),
        auth_time: issued.session.expires - 30 * 24 * 60 * 60,
    };
    let outcome = store
        .run("logout-wrong-session", |tx| {
            snap_oidc::logout_confirm(
                tx,
                &host,
                &config,
                &LogoutConfirm {
                    handle: &handle,
                    origin: ISSUER,
                    session: Some(&other),
                },
                Some(&other),
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(outcome, LogoutConfirmOutcome::InvalidGrant));
    let outcome = store
        .run("logout-confirm", |tx| {
            snap_oidc::logout_confirm(
                tx,
                &host,
                &config,
                &LogoutConfirm {
                    handle: &handle,
                    origin: ISSUER,
                    session: Some(&session),
                },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    let LogoutConfirmOutcome::Redirect { uri } = outcome else {
        panic!("expected logout redirect");
    };
    assert!(uri.starts_with(LOGGED_OUT));
    assert!(uri.contains("state=logout-state"));
}

#[test]
fn concurrent_redemption_has_single_winner() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let first = store
        .run("first", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(first, ExchangeOutcome::Issued(_)));
    let second = store
        .run("second", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &claims(),
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    assert!(matches!(second, ExchangeOutcome::InvalidGrant));
}

#[test]
fn cold_tables_report_terminal_miss() {
    let mut store = cold_store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let result = store.run("cold", |tx| {
        snap_oidc::exchange_code(
            tx,
            &mut host,
            &authority,
            &config,
            &snap_oidc::ExchangeRequest {
                auth: &public_auth(),
                code: "missing",
                redirect_uri: CALLBACK,
                verifier: VERIFIER,
                claims: &claims(),
            },
            NOW,
        )
    });
    assert!(matches!(result, Err(Error::Miss(_))));
}

#[test]
fn raw_material_is_never_retained() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let flows = store
        .run("scan", |tx| tx.find(snap_oidc::TABLES[0], "primary", &[]))
        .unwrap()
        .value;
    let token_rows = store
        .run("scan", |tx| tx.find(snap_oidc::TABLES[2], "primary", &[]))
        .unwrap()
        .value;
    let dump = format!("{flows:?}{token_rows:?}");
    assert!(!dump.contains(&tokens.access));
    assert!(!dump.contains(&tokens.refresh));
}

#[test]
fn token_subject_resolves_without_authority_promise() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    // Continuation handles are not tokens.
    let subject = store
        .run("subject-handle", |tx| {
            snap_oidc::token_subject(tx, &host, &handle)
        })
        .unwrap()
        .value;
    assert_eq!(subject, None);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let subject = store
        .run("subject-code", |tx| {
            snap_oidc::token_subject(tx, &host, &code)
        })
        .unwrap()
        .value;
    assert_eq!(subject.as_deref(), Some(session.subject.as_str()));
    let tokens = exchange(&mut store, &mut host, &authority, &config, &code, NOW);
    // Consumed codes still report their family subject.
    let subject = store
        .run("subject-used", |tx| {
            snap_oidc::token_subject(tx, &host, &code)
        })
        .unwrap()
        .value;
    assert_eq!(subject.as_deref(), Some(session.subject.as_str()));
    for token in [&tokens.access, &tokens.refresh] {
        let subject = store
            .run("subject-token", |tx| {
                snap_oidc::token_subject(tx, &host, token)
            })
            .unwrap()
            .value;
        assert_eq!(subject.as_deref(), Some(session.subject.as_str()));
    }
    let subject = store
        .run("subject-unknown", |tx| {
            snap_oidc::token_subject(tx, &host, "unknown-token")
        })
        .unwrap()
        .value;
    assert_eq!(subject, None);
    let result = store.run("subject-empty", |tx| {
        snap_oidc::token_subject(tx, &host, "")
    });
    assert!(matches!(result, Err(Error::Invalid)));
}

#[test]
fn token_subject_miss_stays_terminal() {
    let mut store = cold_store();
    let host = TestHost::new();
    let result = store.run("cold-subject", |tx| {
        snap_oidc::token_subject(tx, &host, "whatever")
    });
    assert!(matches!(result, Err(Error::Miss(_))));
}

#[test]
fn unavailable_updated_at_is_omitted() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_session, tokens) =
        full_grant(&mut store, &mut host, &authority, &config, &mut crypto, NOW);
    let bare = Claims {
        updated_at: 0,
        ..claims()
    };
    let outcome = store
        .run("userinfo-bare", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &bare,
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    let UserinfoOutcome::Claims(value) = outcome else {
        panic!("expected claims");
    };
    assert_eq!(value["name"], "OIDC Person");
    assert!(value.get("updated_at").is_none());
    // A real timestamp still emits.
    let outcome = store
        .run("userinfo-dated", |tx| {
            snap_oidc::userinfo(
                tx,
                &host,
                &authority,
                &snap_oidc::UserinfoRequest {
                    token: &tokens.access,
                    claims: &claims(),
                },
                NOW + 5,
            )
        })
        .unwrap()
        .value;
    let UserinfoOutcome::Claims(value) = outcome else {
        panic!("expected claims");
    };
    assert_eq!(value["updated_at"], NOW);
    // The same rule applies inside signed ID tokens.
    let (_bearer, session) = enroll_as(&mut store, &mut crypto, "bare@example.test");
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let code = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let outcome = store
        .run("exchange-bare", |tx| {
            snap_oidc::exchange_code(
                tx,
                &mut host,
                &authority,
                &config,
                &snap_oidc::ExchangeRequest {
                    auth: &public_auth(),
                    code: &code,
                    redirect_uri: CALLBACK,
                    verifier: VERIFIER,
                    claims: &bare,
                },
                NOW,
            )
        })
        .unwrap()
        .value;
    let ExchangeOutcome::Issued(bare_tokens) = outcome else {
        panic!("expected issuance");
    };
    let id_claims: serde_json::Value = serde_json::from_str(&bare_tokens.id_token).unwrap();
    assert!(id_claims.get("updated_at").is_none());
}

#[test]
fn consent_details_names_app_and_scopes() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let handle = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let details = store
        .run("details", |tx| {
            snap_oidc::consent_details(tx, &host, &handle)
        })
        .unwrap()
        .value;
    assert_eq!(
        details,
        Some(("chatty".to_string(), "openid profile email".to_string()))
    );
    // Consumed continuations and other kinds report nothing.
    let _ = consent(
        &mut store, &mut host, &authority, &config, &session, &handle, NOW,
    );
    let details = store
        .run("details-used", |tx| {
            snap_oidc::consent_details(tx, &host, &handle)
        })
        .unwrap()
        .value;
    assert_eq!(details, None);
    let outcome = store
        .run("login-flow", |tx| {
            snap_oidc::authorize(
                tx,
                &mut host,
                &authority,
                &config,
                &AuthorizeRequest {
                    max_age: Some("0"),
                    ..authorize_request()
                },
                Some(&session),
                NOW,
            )
        })
        .unwrap()
        .value;
    let AuthorizeOutcome::RequireLogin { handle, .. } = outcome else {
        panic!("expected login continuation");
    };
    let details = store
        .run("details-login", |tx| {
            snap_oidc::consent_details(tx, &host, &handle)
        })
        .unwrap()
        .value;
    assert_eq!(details, None);
    let details = store
        .run("details-unknown", |tx| {
            snap_oidc::consent_details(tx, &host, "unknown-handle")
        })
        .unwrap()
        .value;
    assert_eq!(details, None);
    let result = store.run("details-empty", |tx| {
        snap_oidc::consent_details(tx, &host, "")
    });
    assert!(matches!(result, Err(Error::Invalid)));
    let mut cold = cold_store();
    let result = cold.run("details-cold", |tx| {
        snap_oidc::consent_details(tx, &host, &handle)
    });
    assert!(matches!(result, Err(Error::Miss(_))));
}

#[test]
fn expired_rows_prune() {
    let mut store = store();
    let mut host = TestHost::new();
    let authority = TestAuthority {
        identity: snap_identity::Identity::default(),
    };
    let config = config();
    let mut crypto = IdCrypto::new();
    let (_bearer, session) = enroll(&mut store, &mut crypto);
    let _ = authorize(&mut store, &mut host, &authority, &config, &session, NOW);
    let removed = store
        .run("prune", |tx| snap_oidc::prune_expired(tx, NOW + 301))
        .unwrap()
        .value;
    assert!(removed >= 1);
}
