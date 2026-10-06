//! Thin browser binding of the same Store-backed client used by native agents.
use wasm_bindgen::prelude::*;
fn error(value: impl std::fmt::Debug) -> JsValue {
    JsValue::from_str(&format!("{value:?}"))
}
#[wasm_bindgen]
pub struct ChattyClient {
    client: chatty::client::Client,
}
#[wasm_bindgen]
impl ChattyClient {
    #[wasm_bindgen(constructor)]
    pub fn new(_actor: String) -> Result<Self, JsValue> {
        Ok(Self {
            client: chatty::client::Client::new().map_err(error)?,
        })
    }
    pub fn connect(&mut self, id: &str) -> Result<String, JsValue> {
        serde_json::to_string(&self.client.connect(String::new(), id.into())).map_err(error)
    }
    pub fn invoke(&mut self, operation: &str, input: &str) -> Result<String, JsValue> {
        serde_json::to_string(
            &self
                .client
                .invoke(operation, serde_json::from_str(input).map_err(error)?)
                .map_err(error)?,
        )
        .map_err(error)
    }
    pub fn select(&mut self, thread: Option<String>) -> Result<String, JsValue> {
        let send = self
            .client
            .select(thread)
            .map_err(error)?
            .into_iter()
            .collect();
        self.result(send, None)
    }
    pub fn receive(&mut self, frame: &str) -> Result<String, JsValue> {
        let update = self
            .client
            .receive(serde_json::from_str(frame).map_err(error)?)
            .map_err(error)?;
        self.result(update.send, update.error)
    }
}
impl ChattyClient {
    fn result(
        &mut self,
        commands: Vec<snap_transport::Command>,
        failure: Option<String>,
    ) -> Result<String, JsValue> {
        let send = commands
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let threads = self.client.threads().map_err(error)?;
        let messages = self.client.messages().map_err(error)?;
        serde_json::to_string(
            &serde_json::json!({"threads":threads,"messages":messages,"send":send,
            "error":failure,"ready":self.client.ready()}),
        )
        .map_err(error)
    }
}
#[wasm_bindgen]
pub async fn identity_fetch(origin: String) -> Result<String, JsValue> {
    snap_react_bindings::oauth_fetch(&origin).await
}
