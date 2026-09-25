//! Authy's Workers composition. One configured identity realm is one atomic
//! Store domain, including unique credential claims and cross-client revocation.
use snap_protocol::{Invocation, Operation, Provider};
use snap_store::{Query, Statement, Store as _, Transaction, Value};
use snap_web::{Lease, Reply};
use snap_workers::{
    crypto::{self, Crypto},
    host::{Config, Host},
    store::Store,
};
use std::{cell::OnceCell, rc::Rc};
use worker::*;

struct App(snap_runtime::passport::Passport<Store, Crypto>);
impl Provider for App {
    type Context = Option<String>;
    type Output = Reply;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        self.0.operations()
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        token: Option<String>,
    ) -> impl core::future::Future<
        Output = std::result::Result<snap_protocol::Accepted<Reply>, snap_protocol::Error>,
    > + 'static {
        let future = snap_protocol::dispatch(
            &mut self.0,
            invocation,
            snap_runtime::passport::Context {
                token,
                now: crypto::now(),
            },
        );
        async move {
            Ok(future.await?.map(|reply| Reply {
                outcome: reply.outcome,
                empty: reply.empty,
                cookie: reply.token,
                terminate: reply.revoked,
                lease: reply.session.map(|s| Lease {
                    id: s.session_id,
                    expires_at: s.expires_at,
                }),
            }))
        }
    }
}

#[event(fetch)]
pub async fn fetch(request: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = request.path();
    if path == "/__snap/build"
        || authy::http::ROUTES.contains(&path.as_str())
        || path.starts_with("/identity/")
        || path.starts_with("/account/")
        || path == "/_transport/ws"
    {
        let realm = env.var("SNAP_REALM")?.to_string();
        return env
            .durable_object("IDENTITY")?
            .id_from_name(&realm)?
            .get_stub()?
            .fetch_with_request(request)
            .await;
    }
    env.service("ASSETS")?.fetch_request(request).await
}

#[durable_object]
pub struct IdentityRealm {
    host: Rc<OnceCell<Host<App>>>,
    http: Rc<OnceCell<snap_workers::http::Host<HttpApp>>>,
}
type HttpApp = authy::http::Web<
    Store,
    Crypto,
    snap_store::NoCache,
    snap_workers::oidc::Crypto,
    snap_web::cookie::Cookie,
>;
impl DurableObject for IdentityRealm {
    fn new(state: State, env: Env) -> Self {
        let raw = state._inner();
        let storage = raw.storage().expect("Durable Object storage").into();
        let state = Rc::new(State::from(raw));
        let host = Rc::new(OnceCell::new());
        let http = Rc::new(OnceCell::new());
        let http_output = http.clone();
        let output = host.clone();
        let owner = state.clone();
        let _initialize = state.block_concurrency_while(async move {
            let store = Store::new(storage, &authy::schemas())
                .await
                .map_err(store_error)?;
            let query = Query::new(snap_runtime::passport::SETTINGS)
                .matching(vec![snap_store::Predicate::eq("key", "signing-key")])
                .limit(1);
            let rows = store
                .transaction(Transaction {
                    guards: vec![],
                    statements: vec![Statement::Select(query)],
                })
                .await
                .map_err(store_error)?;
            let persisted =
                if let Some(Value::Text(value)) = rows[0].first().and_then(|r| r.get("value")) {
                    value.clone()
                } else {
                    let value = crypto::random().map_err(|e| Error::RustError(format!("{e:?}")))?;
                    store
                        .transaction(Transaction {
                            guards: vec![],
                            statements: vec![Statement::Insert {
                                table: snap_runtime::passport::SETTINGS,
                                row: [
                                    ("key".into(), "signing-key".into()),
                                    ("value".into(), value.clone().into()),
                                ]
                                .into(),
                            }],
                        })
                        .await
                        .map_err(store_error)?;
                    value
                };
            let key = env
                .secret("SNAP_SESSION_KEY")
                .map(|s| s.to_string())
                .unwrap_or(persisted);
            let origin = env.var("SNAP_ORIGIN")?.to_string();
            let cookie = snap_web::cookie::Cookie::new(
                key.into_bytes(),
                "authy",
                Url::parse(&origin)?.scheme() == "https",
                snap_runtime::passport::SESSION_SECONDS,
            )
            .map_err(|e| Error::RustError(format!("{e:?}")))?;
            let config = Config {
                application: "authy".into(),
                build: env.var("SNAP_BUILD").map(|v| v.to_string()).or_else(|_| {
                    env.get_binding::<WorkerVersionMetadata>("SNAP_VERSION")
                        .map(|v| v.id())
                })?,
                origin: origin.clone(),
                bindings: snap_web::identity("account.create"),
                cookie: cookie.clone(),
                identify: "identity.fetch",
            };
            let signer = snap_workers::oidc::Crypto::load(&store)
                .await
                .map_err(|_| Error::RustError("Cannot initialize OIDC key".into()))?;
            let chatty_origin = env
                .var("CHATTY_ORIGIN")
                .map(|v| v.to_string())
                .unwrap_or_else(|_| "http://127.0.0.1:8789".into());
            let chatty_origin = Url::parse(&chatty_origin)?.origin().ascii_serialization();
            let passport = authy::server(store.clone(), Crypto, snap_store::NoCache);
            let http = authy::http::Web {
                issuer: snap_oidc::Issuer {
                    origin,
                    clients: vec![snap_oidc::Client {
                        id: "chatty".into(),
                        name: "Chatty".into(),
                        redirect_uri: format!("{chatty_origin}/auth/callback"),
                        post_logout_redirect_uri: format!("{chatty_origin}/auth/logged-out"),
                        secret_digest: env
                            .secret("CHATTY_CLIENT_SECRET")
                            .ok()
                            .map(|s| snap_oidc::digest(&s.to_string())),
                    }],
                    store: store.clone(),
                    crypto: signer,
                    accounts: authy::account::Accounts {
                        store,
                        passport: passport.clone(),
                    },
                },
                cookie,
            };
            http_output
                .set(snap_workers::http::Host::new(http, owner.clone()))
                .map_err(|_| Error::RustError("HTTP already initialized".into()))?;
            let application = Host::new(App(passport), owner, config)?;
            output
                .set(application)
                .map_err(|_| Error::RustError("Host already initialized".into()))?;
            Ok(())
        });
        Self { host, http }
    }
    async fn fetch(&self, request: Request) -> Result<Response> {
        if authy::http::ROUTES.contains(&request.path().as_str()) {
            return self
                .http
                .get()
                .ok_or_else(|| Error::RustError("HTTP initialization failed".into()))?
                .fetch(request)
                .await;
        }
        self.host()?.fetch(request).await
    }
    async fn websocket_message(
        &self,
        ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        self.host()?.message(ws, message).await
    }
    async fn websocket_close(
        &self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _clean: bool,
    ) -> Result<()> {
        self.host()?.closed(ws);
        Ok(())
    }
    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.host()?.closed(ws);
        Ok(())
    }
    async fn alarm(&self) -> Result<Response> {
        self.host()?.expire().await?;
        Response::empty()
    }
}
impl IdentityRealm {
    fn host(&self) -> Result<&Host<App>> {
        self.host
            .get()
            .ok_or_else(|| Error::RustError("Host initialization failed".into()))
    }
}
fn store_error(error: snap_store::Error) -> Error {
    Error::RustError(format!("Store failed: {error:?}"))
}
