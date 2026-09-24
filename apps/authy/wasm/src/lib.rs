//! Authy's browser composition. JS marshals commands and immutable observations.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Authy {
    inner: snap_browser::identity::Client,
}

#[wasm_bindgen]
impl Authy {
    #[wasm_bindgen(constructor)]
    pub fn new(base: String, build: String, changed: js_sys::Function) -> Self {
        Self {
            inner: snap_browser::identity::Client::new(
                snap_browser::Http::new(base, build),
                authy::client(),
                move |snapshot| {
                    if let Ok(wire) = serde_json::to_string(snapshot) {
                        let _ = changed.call1(&JsValue::NULL, &JsValue::from_str(&wire));
                    }
                },
            ),
        }
    }
    pub fn snapshot(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.inner.snapshot()).map_err(|e| JsValue::from_str(&e.to_string()))
    }
    pub async fn command(&self, key: String, payload: Option<String>) -> Result<String, JsValue> {
        let payload = payload
            .map(|p| serde_json::from_str(&p))
            .transpose()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let result = self.inner.command(key, payload).await;
        match result {
            Ok(value) => Ok(value.to_string()),
            Err(error) => Err(JsValue::from_str(
                &serde_json::to_string(&error).expect("error"),
            )),
        }
    }
    pub async fn close(&self) {
        self.inner.close().await;
    }
}
