//! Language marshalling and application selection. Browser IO lives in snap-browser.
use futures_util::future::{AbortHandle, Abortable};
use std::{cell::RefCell, collections::BTreeMap};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Client {
    core: RefCell<Option<snap_client::Client>>,
    http: snap_browser::Http,
    pending: RefCell<BTreeMap<String, AbortHandle>>,
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(base: String, build: String) -> Self {
        Self {
            core: RefCell::new(Some(snap_client::Client::default())),
            http: snap_browser::Http::new(base, build),
            pending: RefCell::new(BTreeMap::new()),
        }
    }

    #[wasm_bindgen(js_name = healthUp)]
    pub async fn health_up(&self) -> Result<String, JsValue> {
        let query = self
            .core
            .borrow_mut()
            .as_mut()
            .ok_or_else(|| client_error(snap_browser::failure("Client is closed")))?
            .health_up()
            .map_err(client_error)?;
        let (abort, registration) = AbortHandle::new_pair();
        let id = query.invocation().operation_id.clone();
        self.pending.borrow_mut().insert(id.clone(), abort);
        let result = Abortable::new(self.http.query(query.invocation()), registration).await;
        self.pending.borrow_mut().remove(&id);
        let wire = result
            .map_err(|_| client_error(snap_browser::failure("Client is closed")))?
            .map_err(client_error)?;
        let report = query.complete(&wire).map_err(client_error)?;
        serde_json::to_string(&report).map_err(encoding_error)
    }

    pub fn close(&self) {
        self.core.borrow_mut().take();
        for (_, abort) in std::mem::take(&mut *self.pending.borrow_mut()) {
            abort.abort();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

#[wasm_bindgen]
pub struct Healthy {
    inner: snap_browser::Running<healthy::client::Snapshot>,
}

#[wasm_bindgen]
impl Healthy {
    #[wasm_bindgen(constructor)]
    pub fn new(base: String, build: String, changed: js_sys::Function) -> Self {
        Self {
            inner: snap_browser::start(
                healthy::client::application(),
                snap_browser::Http::new(base, build),
                move |snapshot| {
                    if let Ok(wire) = serde_json::to_string(snapshot) {
                        let _ = changed.call1(&JsValue::NULL, &JsValue::from_str(&wire));
                    }
                },
            ),
        }
    }

    pub fn snapshot(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.inner.snapshot()).map_err(encoding_error)
    }

    pub async fn close(&mut self) {
        self.inner.close().await;
    }
}

fn client_error(error: snap_protocol::Error) -> JsValue {
    match serde_json::to_string(&error) {
        Ok(wire) => JsValue::from_str(&wire),
        Err(error) => encoding_error(error),
    }
}
fn encoding_error(error: serde_json::Error) -> JsValue {
    JsValue::from_str(&error.to_string())
}
