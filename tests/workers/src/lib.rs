//! Private test deployment. No test endpoints are present in application Workers.
use snap_store::{Cache, Query, Rows, Store as _};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use wasm_bindgen::JsValue;
use worker::*;

#[path = "../../adapters/carrier.rs"]
mod carrier;
#[path = "../../store/shared.rs"]
mod shared;

#[derive(Clone, Default)]
struct LocalCache(Rc<RefCell<BTreeMap<Query, Rows>>>);
impl Cache for LocalCache {
    fn get(&self, query: &Query) -> Option<Rows> {
        self.0.borrow().get(query).cloned()
    }
    fn put(&self, query: Query, rows: Rows) {
        self.0.borrow_mut().insert(query, rows);
    }
}

#[event(fetch)]
pub async fn fetch(request: Request, env: Env, _ctx: Context) -> Result<Response> {
    let id = request
        .url()?
        .query_pairs()
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| "contract".into());
    let binding = if matches!(request.path().as_str(), "/contract" | "/read") {
        "STORES"
    } else {
        "CARRIERS"
    };
    env.durable_object(binding)?
        .id_from_name(&id)?
        .get_stub()?
        .fetch_with_request(request)
        .await
}

#[durable_object]
pub struct CarrierContract {
    host: snap_workers::host::Host<carrier::App>,
}
impl DurableObject for CarrierContract {
    fn new(state: State, env: Env) -> Self {
        let config = snap_workers::host::Config {
            application: "carrier-contract".into(),
            build: "healthy-smoke".into(),
            origin: env.var("SNAP_ORIGIN").expect("origin").to_string(),
            bindings: carrier::bindings(),
            cookie: snap_web::cookie::Cookie::new(vec![7; 32], "fixture", false, 60)
                .expect("test cookie"),
            identify: "lease.resolve",
        };
        Self {
            host: snap_workers::host::Host::new(
                carrier::App::new(snap_workers::crypto::now),
                Rc::new(state),
                config,
            )
            .expect("test host"),
        }
    }
    async fn fetch(&self, request: Request) -> Result<Response> {
        self.host.fetch(request).await
    }
    async fn websocket_message(
        &self,
        ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        self.host.message(ws, message).await
    }
    async fn websocket_close(
        &self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _clean: bool,
    ) -> Result<()> {
        self.host.closed(ws);
        Ok(())
    }
    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.host.closed(ws);
        Ok(())
    }
    async fn alarm(&self) -> Result<Response> {
        self.host.expire().await?;
        Response::empty()
    }
}

#[durable_object]
pub struct StoreContract {
    state: State,
    storage: JsValue,
}
impl DurableObject for StoreContract {
    fn new(state: State, _env: Env) -> Self {
        let raw = state._inner();
        let storage = raw.storage().expect("storage binding").into();
        Self {
            state: raw.into(),
            storage,
        }
    }
    async fn fetch(&self, request: Request) -> Result<Response> {
        if request.path() == "/contract" {
            self.state.storage().delete_all().await?;
        }
        let schemas = shared::schemas();
        let store = snap_workers::store::Store::new(self.storage.clone(), &schemas)
            .await
            .map_err(|e| Error::RustError(format!("{e:?}")))?;
        if request.path() == "/contract" {
            shared::contract(store.clone(), store, LocalCache::default()).await;
            return Response::ok("Store contract passed");
        }
        let rows = store
            .transaction(snap_store::Transaction {
                guards: vec![],
                statements: vec![snap_store::Statement::Select(Query::new(schemas[0].table))],
            })
            .await
            .map_err(|e| Error::RustError(format!("{e:?}")))?;
        Response::ok(format!("{rows:?}"))
    }
}
