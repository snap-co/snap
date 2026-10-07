#[allow(dead_code)]
mod support;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use snap_http::{
    FutureValue,
    client::{Body, Client, Incoming, Outgoing},
};
use snap_identity::{
    Crypto, Identity,
    authentication::Authentication,
    oauth::{self, acquisition as flow, verification},
};
use snap_store::Error;
use snap_transport::{
    Invocation, Operation,
    bearer::Change,
    host::{Blocking, Controllers},
    operation::Registry,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

// Controlled entropy and signature capability. These tests exercise Identity's
// orchestration and claim policy, not RS256 implementation or real provider IO.
#[derive(Clone)]
struct ProofCrypto(Arc<AtomicU64>);
impl Crypto for ProofCrypto {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&self.0.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        Ok(bytes)
    }
    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        support::Fake::default().hash_password(password)
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        support::Fake::default().verify_password(password, hash)
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        Sha256::digest(secret.as_bytes()).to_vec()
    }
    fn verify_token(&self, token: &str, jwks: &Value) -> Result<Value, Error> {
        if jwks != &json!({"key":"trusted"}) {
            return Err(Error::Invalid);
        }
        serde_json::from_str(token.strip_prefix("signed:").ok_or(Error::Invalid)?)
            .map_err(|_| Error::Invalid)
    }
}
fn provider(name: &str) -> flow::Provider {
    flow::Provider {
        name: name.into(),
        issuer: "https://authy.test".into(),
        client: name.into(),
        authorization_endpoint: "https://authy.test/authorize".into(),
        continuations: [("app".into(), "https://app.test/callback".into())].into(),
    }
}
fn call<O: Operation>(input: Value) -> Invocation {
    Invocation {
        id: 1,
        operation: O::NAME.into(),
        input,
    }
}

const SECRET: &str = "a-client-secret-with-32-bytes: &%+é";
const CODE: &str = "authorization-code:& +%";

#[derive(Clone)]
struct ProviderHttp {
    requests: Arc<Mutex<Vec<Outgoing>>>,
    nonce: Arc<Mutex<String>>,
    clock: Arc<AtomicI64>,
    scenario: &'static str,
}
struct Chunks(VecDeque<Result<Vec<u8>, String>>);
impl Body for Chunks {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0.pop_front().transpose()
    }
}
impl Client for ProviderHttp {
    type Body = Chunks;
    async fn send(&self, request: Outgoing) -> Result<Incoming<Chunks>, String> {
        let target = request.url.clone();
        self.requests.lock().unwrap().push(request);
        let scenario = self.scenario;
        let mut status = 200;
        let value = match target.as_str() {
            "https://authy.test/.well-known/openid-configuration" => {
                if scenario == "discovery-redirect" {
                    status = 302;
                }
                let mut metadata = json!({
                    "issuer":"https://authy.test", "authorization_endpoint":"https://authy.test/authorize",
                    "token_endpoint":"https://authy.test/token", "jwks_uri":"https://authy.test/keys", "userinfo_endpoint":"https://authy.test/profile",
                });
                match scenario {
                    "issuer" => metadata["issuer"] = json!("https://attacker.test"),
                    "authorization-endpoint" => {
                        metadata["authorization_endpoint"] =
                            json!("https://authy.test/other-authorization")
                    }
                    "token-endpoint" => {
                        metadata["token_endpoint"] = json!("https://authy.test.attacker.test/token")
                    }
                    "keys-endpoint" => metadata["jwks_uri"] = json!("https://attacker.test/keys"),
                    "profile-endpoint" => {
                        metadata["userinfo_endpoint"] = json!("https://attacker.test/profile")
                    }
                    "userinfo-url" => {
                        metadata["token_endpoint"] = json!("https://user:password@authy.test/token")
                    }
                    "fragment-url" => {
                        metadata["token_endpoint"] = json!("https://authy.test/token#fragment")
                    }
                    _ => {}
                }
                metadata
            }
            "https://authy.test/token" => {
                if scenario == "uncertain" {
                    return Err("response lost after sending code".into());
                }
                if scenario == "token-redirect" {
                    status = 302;
                }
                let nonce = self.nonce.lock().unwrap().clone();
                let claims = json!({"iss":"https://authy.test", "aud":"authy", "sub":if scenario == "subject" { "intruder" } else { "person" }, "iat":100, "exp":if scenario == "long-proof" { 1000 } else { 200 }, "auth_time":if scenario == "auth-time" { 91 } else { 90 },
                    "nonce": if scenario == "nonce" { "wrong-nonce" } else { &nonce }});
                let token = format!(
                    "{}{}",
                    if scenario == "signature" {
                        "unsigned:"
                    } else {
                        "signed:"
                    },
                    claims
                );
                json!({"id_token":token, "access_token":"private-access", "refresh_token":"private-refresh", "token_type":"Bearer", "expires_in":60})
            }
            "https://authy.test/keys" => {
                if scenario == "keys-failure" {
                    return Err("keys unavailable".into());
                }
                json!({"key":"trusted"})
            }
            "https://authy.test/profile" => {
                match scenario {
                    "slow-profile" => self.clock.store(120, Ordering::Relaxed),
                    "proof-expired" => self.clock.store(201, Ordering::Relaxed),
                    "access-expired" => self.clock.store(161, Ordering::Relaxed),
                    "clock-backwards" => self.clock.store(99, Ordering::Relaxed),
                    _ => {}
                }
                json!({"sub": if scenario == "profile" { "another-person" } else { "person" }})
            }
            _ => panic!("unexpected provider destination"),
        };
        let bytes = serde_json::to_vec(&value).unwrap();
        let chunks = if scenario == "oversize" {
            vec![Ok(vec![b' '; 256 * 1024]), Ok(vec![b' '; 1])]
        } else if scenario == "bad-json" {
            vec![Ok(b"not-json".to_vec())]
        } else if scenario == "chunk-failure" {
            vec![Ok(bytes), Err("stream interrupted".into())]
        } else {
            // Normal responses are incremental, not an assumed single chunk.
            let (first, second) = bytes.split_at(bytes.len() / 2);
            vec![Ok(first.to_vec()), Ok(second.to_vec())]
        };
        Ok(Incoming {
            status,
            headers: vec![],
            body: Chunks(chunks.into()),
        })
    }
}
fn ready(mut future: FutureValue<Result<Value, Error>>) -> Result<Value, Error> {
    match future
        .as_mut()
        .poll(&mut Context::from_waker(std::task::Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("controlled HTTP must not wait on physical IO"),
    }
}

type TestHost = Blocking<snap_store_sqlite::Sqlite, Controllers<snap_store_sqlite::Sqlite, ()>>;
struct Fixture {
    host: TestHost,
    clock: Arc<AtomicI64>,
    requests: Arc<Mutex<Vec<Outgoing>>>,
    nonce: Arc<Mutex<String>>,
}
fn fixture(scenario: &'static str, renewal: Option<&'static str>) -> Fixture {
    let mut migrations = support::migrations();
    migrations.push(toml::from_str(snap_store::resource::MIGRATION).unwrap());
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    let crypto = ProofCrypto(Arc::new(AtomicU64::new(1)));
    let factory = crypto.clone();
    let mut definitions = flow::definitions(
        Identity::new(1000).unwrap(),
        vec![provider("authy"), provider("other")],
        move || factory.clone(),
    )
    .unwrap()
    .preconnection;
    let factory = crypto.clone();
    definitions.extend(
        snap_identity::operation::definitions(Identity::default(), move || factory.clone(), None)
            .preconnection,
    );
    let factory = crypto.clone();
    definitions.extend(
        oauth::release::definitions(
            vec![provider("authy")],
            [("app".into(), "https://app.test/logged-out".into())].into(),
            move || factory.clone(),
        )
        .unwrap(),
    );
    let operations = definitions
        .into_iter()
        .fold(Registry::default(), |r, d| r.with_preconnection_request(d));
    let clock = Arc::new(AtomicI64::new(100));
    let now = clock.clone();
    let authority = Authentication::new(
        Arc::new(Identity::default().provider(crypto.clone())),
        Arc::new(move || now.load(Ordering::Relaxed)),
    );
    let http = ProviderHttp {
        requests: Default::default(),
        nonce: Default::default(),
        clock: clock.clone(),
        scenario,
    };
    let mut controllers = Controllers::around(());
    if scenario != "interrupted" {
        let verifier = crypto.clone();
        let time = clock.clone();
        controllers = controllers.with_controller(
            verification::controller(
                vec![provider("authy"), provider("other")],
                http.clone(),
                move || verifier.clone(),
                move |name| {
                    assert_eq!(name, "authy");
                    if scenario == "secret-failure" {
                        Err(Error::Unavailable)
                    } else {
                        Ok(SECRET.into())
                    }
                },
                move || time.load(Ordering::Relaxed),
                ready,
            )
            .unwrap(),
        );
    }
    if let Some(scenario) = renewal {
        let verifier = crypto.clone();
        let time = clock.clone();
        let mut renewal_http = http.clone();
        renewal_http.scenario = scenario;
        controllers = controllers.with_controller(
            verification::renewal_controller(
                vec![provider("authy")],
                renewal_http,
                move || verifier.clone(),
                |_| Ok(SECRET.into()),
                move || time.load(Ordering::Relaxed),
                ready,
            )
            .unwrap(),
        );
    }
    let time = clock.clone();
    let host = Blocking::new(
        store,
        controllers,
        operations,
        Arc::new(authority),
        Default::default(),
        "oauth-test".into(),
    )
    .with_inputs(move |key| {
        if key == "clock" {
            Ok(json!(time.load(Ordering::Relaxed)))
        } else {
            Err(snap_transport::Error::Unavailable)
        }
    });
    Fixture {
        host,
        clock,
        requests: http.requests,
        nonce: http.nonce,
    }
}

impl Fixture {
    fn begin<O: Operation>(&mut self, bearer: Option<&str>) -> Value {
        let value = self
            .host
            .preconnection_reply(
                call::<O>(json!({"provider":"authy","continuation":"app"})),
                bearer.map(str::to_owned),
            )
            .outcome
            .unwrap();
        *self.nonce.lock().unwrap() = value["nonce"].as_str().unwrap().into();
        value
    }
    fn callback(&mut self, value: &Value) -> snap_transport::bearer::Reply {
        self.host.preconnection_reply(call::<flow::Callback>(json!({"provider":"authy", "state":value["state"], "binding":value["binding"], "issuer":"https://authy.test", "response":{"kind":"code","code":CODE}})), None)
    }
    fn login(&mut self) -> String {
        let value = self.begin::<flow::Begin>(None);
        let reply = self.callback(&value);
        assert!(reply.outcome.is_ok());
        match reply.bearer.unwrap() {
            Change::Set(token) => token.expose().into(),
            _ => panic!("missing issued bearer"),
        }
    }
}

#[test]
fn acquisition_dispatch_keeps_proof_private_and_fences_failed_replayed_or_interrupted_exchanges() {
    for scenario in [
        "success",
        "slow-profile",
        "signature",
        "nonce",
        "profile",
        "uncertain",
        "rejected",
        "expired",
        "interrupted",
        "issuer",
        "authorization-endpoint",
        "token-endpoint",
        "keys-endpoint",
        "profile-endpoint",
        "userinfo-url",
        "fragment-url",
        "discovery-redirect",
        "token-redirect",
        "keys-failure",
        "oversize",
        "bad-json",
        "chunk-failure",
        "proof-expired",
        "access-expired",
        "clock-backwards",
        "secret-failure",
    ] {
        let Fixture {
            mut host,
            clock,
            requests,
            nonce,
            ..
        } = fixture(scenario, None);
        let start = host.preconnection_reply(
            call::<flow::Begin>(json!({"provider":"authy", "continuation":"app"})),
            None,
        );
        assert!(start.accepted);
        assert!(start.bearer.is_none());
        let authorization = start.outcome.unwrap();
        *nonce.lock().unwrap() = authorization["nonce"].as_str().unwrap().into();
        assert_eq!(authorization["endpoint"], "https://authy.test/authorize");
        let code = json!({"kind":"code", "code":CODE});
        let mut callback = json!({"provider":"authy", "state":authorization["state"], "binding":authorization["binding"], "issuer":"https://authy.test", "response":code});
        for (field, invalid) in [
            ("binding", "wrong-browser"),
            ("provider", "other"),
            ("issuer", "https://attacker.test"),
        ] {
            let mut wrong = callback.clone();
            wrong[field] = json!(invalid);
            let denied = host.preconnection_reply(call::<flow::Callback>(wrong), None);
            assert!(denied.outcome.is_err());
            assert!(denied.bearer.is_none());
            assert!(requests.lock().unwrap().is_empty());
        }
        let mut malformed = callback.clone();
        malformed["response"] = json!({"kind":"code"});
        assert!(
            !host
                .preconnection_reply(call::<flow::Callback>(malformed), None)
                .accepted
        );
        if scenario == "expired" {
            clock.store(401, Ordering::Relaxed);
        }
        if scenario == "rejected" {
            callback["response"] = json!({"kind":"rejected", "error":"access_denied"});
        }
        let reply = host.preconnection_reply(call::<flow::Callback>(callback.clone()), None);
        if matches!(scenario, "success" | "slow-profile") {
            let principal: snap_identity::Principal =
                serde_json::from_value(reply.outcome.unwrap()).unwrap();
            assert_eq!(
                principal.identity,
                oauth::owner("https://authy.test", "person")
            );
            let Some(Change::Set(token)) = reply.bearer else {
                panic!("missing bearer after completion");
            };
            let persisted = host
                .transact("session.read", |tx| {
                    oauth::resolve(tx, token.expose(), clock.load(Ordering::Relaxed))
                })
                .unwrap();
            assert_eq!(persisted.subject, "person");
            assert_eq!(persisted.tokens.refresh, "private-refresh");
            assert_eq!(
                persisted.tokens.access_expires, 160,
                "profile IO must not extend upstream validity"
            );
        } else {
            assert!(reply.outcome.is_err(), "{scenario}");
            assert!(reply.bearer.is_none(), "{scenario}");
            let sessions = host
                .transact("sessions.read", |tx| {
                    tx.find("identity.sessions", "primary", &[])
                })
                .unwrap();
            assert!(sessions.is_empty(), "{scenario}");
        }
        let expected = match scenario {
            "rejected" | "expired" | "interrupted" | "secret-failure" => 0,
            "uncertain" | "token-redirect" => 2,
            "signature" | "nonce" | "keys-failure" => 3,
            "success" | "slow-profile" | "profile" | "proof-expired" | "access-expired"
            | "clock-backwards" => 4,
            _ => 1,
        };
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), expected, "{scenario}");
            for request in requests.iter() {
                assert_eq!(request.max_bytes, 256 * 1024);
                assert_eq!(request.timeout_ms, 30_000);
                if request.url.ends_with("/token") {
                    assert_eq!(request.method, "POST");
                    let form: std::collections::BTreeMap<_, _> =
                        url::form_urlencoded::parse(&request.body)
                            .into_owned()
                            .collect();
                    assert_eq!(form["grant_type"], "authorization_code");
                    assert_eq!(form["code"], CODE);
                    assert_eq!(form["redirect_uri"], "https://app.test/callback");
                    assert_eq!(
                        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
                        authorization["code_challenge"]
                    );
                    let basic = request
                        .headers
                        .iter()
                        .find(|(name, _)| name == "authorization")
                        .unwrap()
                        .1
                        .strip_prefix("Basic ")
                        .unwrap();
                    let credential = String::from_utf8(STANDARD.decode(basic).unwrap()).unwrap();
                    assert_eq!(
                        credential,
                        "authy:a-client-secret-with-32-bytes%3A+%26%25%2B%C3%A9"
                    );
                } else {
                    assert_eq!(request.method, "GET");
                    assert!(request.body.is_empty());
                    let credential = request
                        .headers
                        .iter()
                        .find(|(name, _)| name == "authorization");
                    if request.url.ends_with("/profile") {
                        assert_eq!(credential.unwrap().1, "Bearer private-access");
                    } else {
                        assert!(credential.is_none());
                    }
                }
            }
        }
        if scenario == "interrupted" {
            host.transact("recovery", |tx| oauth::recover(tx, 100))
                .unwrap();
        }
        let replay = host.preconnection_reply(call::<flow::Callback>(callback), None);
        assert!(replay.outcome.is_err());
        assert!(replay.bearer.is_none());
        assert_eq!(requests.lock().unwrap().len(), expected);
    }
}

#[test]
fn linking_captures_fresh_identity_and_logout_commits_revocation_before_return() {
    use oauth::release;
    for scenario in ["linked", "revoked", "stale"] {
        let mut f = fixture(
            if scenario == "stale" {
                "long-proof"
            } else {
                "success"
            },
            None,
        );
        let password = f.host.preconnection_reply(
            call::<snap_identity::operation::Enroll>(
                json!({"email":"local@example.test","password":"password1"}),
            ),
            None,
        );
        let principal = serde_json::from_value(password.outcome.unwrap()).unwrap();
        let Some(Change::Set(bearer)) = password.bearer else {
            panic!("missing password bearer");
        };
        let password = snap_identity::Issued {
            principal,
            bearer: bearer.expose().into(),
        };
        if scenario == "stale" {
            f.clock.store(101, Ordering::Relaxed);
        }
        let challenge = f.begin::<flow::Link>(Some(&password.bearer));
        if scenario == "revoked" {
            f.host
                .transact("revoke", |tx| oauth::revoke(tx, &password.bearer))
                .unwrap();
        } else if scenario == "stale" {
            // Keep the provider proof/attempt valid but let the original local
            // authentication lose its fresh-proof window before completion.
            f.clock.store(400, Ordering::Relaxed);
        }
        let reply = f.callback(&challenge);
        if scenario != "linked" {
            assert!(reply.outcome.is_err());
            assert!(reply.bearer.is_none());
            assert_eq!(
                f.requests.lock().unwrap().len(),
                4,
                "proof verified before local linking recheck"
            );
            continue;
        }
        let principal: snap_identity::Principal =
            serde_json::from_value(reply.outcome.unwrap()).unwrap();
        assert_eq!(principal.identity, password.principal.identity);
        let Change::Set(token) = reply.bearer.unwrap() else {
            panic!("missing linked session");
        };
        let bearer = token.expose().to_owned();
        let grant = f
            .host
            .transact("linked grant", |tx| oauth::resolve(tx, &bearer, 100))
            .unwrap();
        assert_eq!(grant.owner, password.principal.identity);
        let wrong = f.host.preconnection_reply(
            call::<release::Release>(
                json!({"provider":"authy","continuation":"app","csrf":"wrong"}),
            ),
            Some(bearer.clone()),
        );
        assert!(wrong.outcome.is_err());
        assert!(wrong.bearer.is_none());
        assert!(
            f.host
                .transact("still valid", |tx| oauth::resolve(tx, &bearer, 100))
                .is_ok()
        );
        let reply = f.host.preconnection_reply(
            call::<release::Release>(
                json!({"provider":"authy","continuation":"app","csrf":grant.csrf}),
            ),
            Some(bearer.clone()),
        );
        let value = reply.outcome.unwrap();
        assert!(matches!(reply.bearer, Some(Change::Clear)));
        assert!(
            f.host
                .transact("revoked before upstream return", |tx| oauth::retained(
                    tx, &grant.id, 100
                ))
                .is_err()
        );
        let returned = json!({"state":value["state"],"binding":value["binding"]});
        let mut wrong = returned.clone();
        wrong["binding"] = json!("wrong");
        assert!(
            f.host
                .preconnection_reply(call::<release::Returned>(wrong), None)
                .outcome
                .is_err()
        );
        assert!(
            f.host
                .preconnection_reply(call::<release::Returned>(returned.clone()), None)
                .outcome
                .is_ok()
        );
        assert!(
            f.host
                .preconnection_reply(call::<release::Returned>(returned), None)
                .outcome
                .is_err()
        );
        assert_eq!(
            f.requests.lock().unwrap().len(),
            4,
            "logout does not perform provider IO"
        );
    }
}

#[test]
fn renewal_preserves_local_lifetime_and_fences_uncertainty_without_replay() {
    for scenario in [
        "success",
        "uncertain",
        "signature",
        "subject",
        "auth-time",
        "keys-failure",
        "issuer",
        "expired-local",
        "interrupted",
    ] {
        let mut f = fixture("success", (scenario != "interrupted").then_some(scenario));
        let bearer = f.login();
        let grant = f
            .host
            .transact("grant", |tx| oauth::resolve(tx, &bearer, 100))
            .unwrap();
        f.requests.lock().unwrap().clear();
        f.host
            .transact("fresh", |tx| oauth::renewal::request(tx, &grant.id, 100))
            .unwrap();
        assert!(f.requests.lock().unwrap().is_empty());
        let now = if scenario == "expired-local" {
            grant.expires
        } else {
            130
        };
        f.clock.store(now, Ordering::Relaxed);
        let result = f
            .host
            .transact("renewal", |tx| oauth::renewal::request(tx, &grant.id, now));
        if scenario == "expired-local" {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        if scenario == "interrupted" {
            f.host
                .transact("recover", |tx| oauth::recover(tx, now))
                .unwrap();
        }
        let refreshed = f
            .host
            .transact("after", |tx| oauth::resolve(tx, &bearer, now));
        if scenario == "success" {
            let refreshed = refreshed.unwrap();
            assert_eq!(refreshed.expires, grant.expires);
            assert_eq!(refreshed.tokens.auth_time, grant.tokens.auth_time);
            assert_eq!(refreshed.version, grant.version + 1);
            assert_eq!(refreshed.tokens.access_expires, 190);
        } else {
            assert!(refreshed.is_err(), "{scenario}");
        }
        let count = f.requests.lock().unwrap().len();
        let again = f.host.transact("never replay", |tx| {
            oauth::renewal::request(tx, &grant.id, now)
        });
        assert_eq!(again.is_ok(), scenario == "success");
        assert_eq!(f.requests.lock().unwrap().len(), count);
        assert_eq!(
            count,
            match scenario {
                "success" | "signature" | "subject" | "auth-time" | "keys-failure" => 3,
                "uncertain" => 2,
                "issuer" => 1,
                _ => 0,
            },
            "{scenario}"
        );
        for request in f
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
        {
            let form: std::collections::BTreeMap<_, _> = url::form_urlencoded::parse(&request.body)
                .into_owned()
                .collect();
            assert_eq!(form["grant_type"], "refresh_token");
            assert_eq!(form["refresh_token"], "private-refresh");
        }
    }
}

#[path = "../../transport/tests/support/mod.rs"]
mod tls_support;

#[tokio::test]
async fn tcp_acquisition_publishes_reusable_bearer_without_cookie_shaped_inputs() {
    use snap_transport::native::{
        TcpClient,
        driver::{Dispatcher, Shared},
    };
    use snap_transport::{Command, Event, Response, carrier::Dispatch};
    let mut f = fixture("success", None);
    f.host.recover().unwrap();
    let shared = Shared::new(f.host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let directory = tempfile::tempdir().unwrap();
    let (server_tls, client_tls) = tls_support::pki(directory.path(), false);
    let serving = tokio::spawn(snap_transport::native::tcp::serve(
        listener,
        Dispatcher::tcp(shared.clone(), None),
        server_tls,
    ));
    let pump = tokio::spawn(snap_transport::native::driver::dispatch(shared.clone()));
    struct Stop<T>(tokio::task::JoinHandle<T>);
    impl<T> Drop for Stop<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _serving = Stop(serving);
    let _pump = Stop(pump);
    async fn request(
        address: &str,
        tls: &snap_transport::native::tls::ClientTls,
        command: Command,
    ) -> (Result<Value, snap_transport::Error>, Option<Change>) {
        let mut client = TcpClient::open(address, tls).await.unwrap();
        client.send(&command).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut accepted = false;
            let mut bearer = None;
            loop {
                match client.receive().await.unwrap().0 {
                    Response::Event(Event::Accepted { .. }) => accepted = true,
                    Response::Event(Event::Bearer { change, .. }) => {
                        assert!(accepted);
                        assert!(bearer.is_none());
                        bearer = Some(change);
                    }
                    Response::Event(Event::Completed { outcome, .. }) => return (outcome, bearer),
                    Response::Failed(error) => return (Err(error), bearer),
                    other => panic!("unexpected acquisition frame {other:?}"),
                }
            }
        })
        .await
        .unwrap()
    }
    let (authorization, bearer) = request(
        &address,
        &client_tls,
        Command::Request {
            bearer: None,
            invocation: call::<flow::Begin>(json!({"provider":"authy","continuation":"app"})),
        },
    )
    .await;
    assert!(bearer.is_none());
    let authorization = authorization.unwrap();
    *f.nonce.lock().unwrap() = authorization["nonce"].as_str().unwrap().into();
    let callback = call::<flow::Callback>(
        json!({"provider":"authy","state":authorization["state"],"binding":authorization["binding"],"issuer":"https://authy.test","response":{"kind":"code","code":CODE}}),
    );
    let (principal, bearer) = request(
        &address,
        &client_tls,
        Command::Request {
            bearer: None,
            invocation: callback.clone(),
        },
    )
    .await;
    assert_eq!(
        principal.unwrap()["identity"],
        oauth::owner("https://authy.test", "person")
    );
    let Some(Change::Set(token)) = bearer else {
        panic!("missing committed bearer");
    };
    let mut lifetime = None;
    for turn in 0..2 {
        let (mut connected, info) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let mut connected = TcpClient::open(&address, &client_tls).await.unwrap();
                    connected
                        .send(&Command::Connect {
                            bearer: token.expose().into(),
                            client_id: "native-login".into(),
                        })
                        .await
                        .unwrap();
                    let (reply, info) = connected.receive().await.unwrap();
                    match reply {
                        Response::Attached { resumed } => {
                            assert_eq!(resumed, turn > 0);
                            break (connected, info.unwrap());
                        }
                        Response::Failed(snap_transport::Error::Occupied) => {
                            tokio::task::yield_now().await
                        }
                        other => panic!("unexpected connection reply {other:?}"),
                    }
                }
            })
            .await
            .unwrap();
        if let Some(old) = &lifetime {
            assert_eq!(old, &info);
        }
        lifetime = Some(info);
        connected.send(&Command::Disconnect).await.unwrap();
        assert!(
            connected.receive().await.is_err(),
            "TCP detach retires the physical stream"
        );
    }
    let (replay, bearer) = request(
        &address,
        &client_tls,
        Command::Request {
            bearer: None,
            invocation: callback,
        },
    )
    .await;
    assert!(replay.is_err());
    assert!(bearer.is_none());
    assert_eq!(f.requests.lock().unwrap().len(), 4);
    // HTTP and TCP use the same captured declaration, not a separate provider executor.
    let reply = Dispatcher::web(shared)
        .request(
            call::<flow::Begin>(json!({"provider":"authy","continuation":"app"})),
            Some(token.expose().into()),
        )
        .await;
    assert!(reply.outcome.is_ok());
}
