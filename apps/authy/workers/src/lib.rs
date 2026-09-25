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
    fn invoke(
        &mut self,
        invocation: Invocation,
        token: Option<String>,
    ) -> impl core::future::Future<Output = Reply> + 'static {
        let future = self.0.invoke(
            invocation,
            snap_runtime::passport::Context {
                token,
                now: crypto::now(),
            },
        );
        async move {
            let reply = future.await;
            Reply {
                outcome: reply.outcome,
                empty: reply.empty,
                cookie: reply.token,
                terminate: reply.revoked,
                lease: reply.session.map(|s| Lease {
                    id: s.session_id,
                    expires_at: s.expires_at,
                }),
            }
        }
    }
}

#[event(fetch)]
pub async fn fetch(request: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = request.path();
    if path == "/__snap/build"
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
}
impl DurableObject for IdentityRealm {
    fn new(state: State, env: Env) -> Self {
        let raw = state._inner();
        let storage = raw.storage().expect("Durable Object storage").into();
        let state = Rc::new(State::from(raw));
        let host = Rc::new(OnceCell::new());
        let output = host.clone();
        let owner = state.clone();
        let _initialize = state.block_concurrency_while(async move {
            let store = Store::new(storage, &snap_runtime::passport::schemas())
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
                origin,
                bindings: snap_web::identity("account.create"),
                cookie,
                identify: "identity.fetch",
            };
            let application = Host::new(
                App(authy::server(store, Crypto, snap_store::NoCache)),
                owner,
                config,
            )?;
            output
                .set(application)
                .map_err(|_| Error::RustError("Host already initialized".into()))?;
            Ok(())
        });
        Self { host }
    }
    async fn fetch(&self, request: Request) -> Result<Response> {
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
