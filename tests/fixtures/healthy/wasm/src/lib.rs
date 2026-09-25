//! Healthy's browser composition and application-specific exports.
pub use snap_client_wasm::Client;
use wasm_bindgen::prelude::*;

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
        serde_json::to_string(&self.inner.snapshot())
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    pub async fn close(&mut self) {
        self.inner.close().await;
    }
}
