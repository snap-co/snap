//! One local Chatty realm keeps sessions and owner-scoped thread transactions in
//! one Durable Object. Runtime loss marks work interrupted, without replaying IO.
use chatty::storage::{SETTINGS, read, row, text, tx};
use snap_http::{
    FutureValue,
    client::{Client, Incoming, Outgoing},
};
use snap_workers::{crypto, store::Store};
use std::{cell::OnceCell, rc::Rc, time::Duration};
use worker::*;

#[derive(Clone)]
pub struct Workers {
    http: snap_workers::outgoing::Http,
    state: Rc<State>,
}
impl Client for Workers {
    type Body = <snap_workers::outgoing::Http as Client>::Body;
    async fn send(&self, request: Outgoing) -> core::result::Result<Incoming<Self::Body>, String> {
        self.http.send(request).await
    }
}
impl chatty::Host for Workers {
    fn now(&self) -> u64 {
        crypto::now()
    }
    fn random(&self) -> core::result::Result<String, snap_http::Response> {
        crypto::random().map_err(|_| chatty::storage::unavailable())
    }
    async fn verify(
        &self,
        token: String,
        jwks: serde_json::Value,
    ) -> core::result::Result<serde_json::Value, snap_http::Response> {
        snap_workers::oidc::verify(&token, &jwks).await
    }
    fn spawn(&self, future: FutureValue<()>) {
        self.state.wait_until(future);
    }
    async fn sleep(&self, milliseconds: u64) {
        Delay::from(Duration::from_millis(milliseconds)).await;
    }
    async fn files(
        &self,
        _owner: String,
        _request: chatty::FileRequest,
    ) -> core::result::Result<serde_json::Value, String> {
        Err("File tools are available on the native host".into())
    }
}
#[event(fetch)]
pub async fn fetch(request: Request, env: Env, _context: Context) -> Result<Response> {
    if chatty::web::ROUTES.contains(&request.path().as_str()) {
        return env
            .durable_object("CHATTY")?
            .id_from_name(&value(&env, "SNAP_REALM", "chatty"))?
            .get_stub()?
            .fetch_with_request(request)
            .await;
    }
    env.service("ASSETS")?.fetch_request(request).await
}
type App = chatty::web::Web<Store, Workers, snap_web::cookie::Cookie>;
#[durable_object]
pub struct ChattyRealm {
    host: Rc<OnceCell<snap_workers::http::Host<App>>>,
}
impl DurableObject for ChattyRealm {
    fn new(state: State, env: Env) -> Self {
        let raw = state._inner();
        let storage = raw.storage().expect("Durable Object storage").into();
        let state = Rc::new(State::from(raw));
        let host = Rc::new(OnceCell::new());
        let output = host.clone();
        let owner = state.clone();
        let _initialize = state.block_concurrency_while(async move {
            let store = Store::new(storage, &chatty::schemas())
                .await
                .map_err(|_| failure("Cannot initialize Chatty storage"))?;
            chatty::storage::recover(&store, crypto::now())
                .await
                .map_err(|_| failure("Cannot recover Chatty work"))?;
            let q = snap_store::Query::new(SETTINGS)
                .matching(vec![snap_store::Predicate::eq("key", "cookie")])
                .limit(1);
            let key = if let Some(record) = read(&store, q)
                .await
                .map_err(|_| failure("Cannot load Chatty key"))?
            {
                text(&record, "value").map_err(|_| failure("Invalid Chatty key"))?
            } else {
                let key = crypto::random().map_err(|_| failure("Cannot generate Chatty key"))?;
                tx(
                    &store,
                    vec![],
                    vec![snap_store::Statement::Insert {
                        table: SETTINGS,
                        row: row(&[("key", "cookie".into()), ("value", key.clone().into())]),
                    }],
                )
                .await
                .map_err(|_| failure("Cannot persist Chatty key"))?;
                key
            };
            let origin = Url::parse(&value(&env, "SNAP_ORIGIN", "http://127.0.0.1:8789"))?
                .origin()
                .ascii_serialization();
            let issuer = Url::parse(&value(&env, "AUTHY_ORIGIN", "http://127.0.0.1:8788"))?
                .origin()
                .ascii_serialization();
            let secure = origin.starts_with("https:");
            let cookie = snap_web::cookie::Cookie::new(
                key.as_bytes().to_vec(),
                "chatty",
                secure,
                30 * 24 * 3600,
            )
            .map_err(|_| failure("Invalid cookie key"))?;
            let correlation =
                snap_web::cookie::Cookie::new(key.into_bytes(), "chatty_login", secure, 300)
                    .map_err(|_| failure("Invalid cookie key"))?;
            let http = if value(&env, "CHATTY_AUTHY_HTTP", "") == "1" {
                snap_workers::outgoing::Http::default()
            } else {
                snap_workers::outgoing::Http::with_authy(issuer.clone(), env.service("AUTHY")?)
            };
            let host = Workers {
                http,
                state: owner.clone(),
            };
            let config = chatty::Config {
                origin,
                issuer,
                client_id: "chatty".into(),
                client_secret: secret(&env, "CHATTY_CLIENT_SECRET"),
                model: snap_llm::Config {
                    endpoint: value(
                        &env,
                        "CHATTY_MODEL_ENDPOINT",
                        "https://opencode.ai/zen/go/v1/responses",
                    ),
                    model: value(&env, "CHATTY_MODEL", "muse-spark-1.3-contributor"),
                    key: secret(&env, "OPENCODE_API_KEY"),
                    max_output_tokens: 8192,
                },
                exa_key: secret(&env, "EXA_API_KEY"),
                files: false,
            };
            output
                .set(snap_workers::http::Host::new(
                    chatty::web::Web::new(store, host, config, cookie, correlation),
                    owner,
                ))
                .map_err(|_| failure("Chatty initialized twice"))?;
            Ok(())
        });
        Self { host }
    }
    async fn fetch(&self, request: Request) -> Result<Response> {
        self.host
            .get()
            .ok_or_else(|| failure("Chatty initialization failed"))?
            .fetch(request)
            .await
    }
}
fn value(env: &Env, key: &str, default: &str) -> String {
    env.var(key)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| default.into())
}
fn secret(env: &Env, key: &str) -> String {
    env.secret(key).map(|s| s.to_string()).unwrap_or_default()
}
fn failure(message: &str) -> Error {
    Error::RustError(message.into())
}
