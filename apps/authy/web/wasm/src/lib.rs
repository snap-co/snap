//! Thin browser binding over Authy's portable Store replica client. JavaScript
//! owns sockets and forms. Rust owns program execution and invocation correlation.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct AuthyClient {
    client: authy::client::Profiles,
}

fn error(error: impl core::fmt::Debug) -> JsValue {
    JsValue::from_str(&format!("{error:?}"))
}
fn command(command: &snap_transport::Command) -> Result<String, JsValue> {
    serde_json::to_string(command).map_err(error)
}

impl AuthyClient {
    fn result(&mut self, send: Vec<String>, failure: Option<String>) -> Result<String, JsValue> {
        let profile = self.client.profile().map_err(error)?.map(|profile| {
            serde_json::json!({
                "revision":profile.revision.to_string(),
                "value":{"first_name":profile.first_name,"last_name":profile.last_name},
            })
        });
        serde_json::to_string(&serde_json::json!({
            "snapshot": {
                "profile":profile, "pending":self.client.pending(),
                "revision":self.client.sequence().to_string(),
                "reconciling":!self.client.ready(), "needs_recovery":false,
            },
            "send":send, "error":failure,
        }))
        .map_err(error)
    }
}

#[wasm_bindgen]
impl AuthyClient {
    #[wasm_bindgen(constructor)]
    pub fn new(_actor: String, profile: String) -> Result<Self, JsValue> {
        Ok(Self {
            client: authy::client::Profiles::new(profile).map_err(error)?,
        })
    }
    pub fn invoke(&mut self, operation: &str, input: &str) -> Result<String, JsValue> {
        let input = serde_json::from_str(input).map_err(error)?;
        command(&self.client.invoke(operation, input).map_err(error)?)
    }
    pub fn connect_command(&mut self, id: &str) -> Result<String, JsValue> {
        command(&self.client.connect(id).map_err(error)?)
    }
    pub fn enqueue_edit(&mut self, first: &str, last: &str) -> Result<String, JsValue> {
        let command = self.client.edit(first, last).map_err(error)?;
        self.result(vec![crate::command(&command)?], None)
    }
    pub fn receive(&mut self, frame: &str) -> Result<String, JsValue> {
        let response = serde_json::from_str(frame).map_err(error)?;
        let update = self.client.receive(response).map_err(error)?;
        let send = update.send.iter().map(command).collect::<Result<_, _>>()?;
        self.result(send, update.error)
    }
}

snap_react_bindings::export_identity!();
#[wasm_bindgen]
pub async fn account_fetch() -> Result<String, JsValue> {
    let mut transport = snap_transport::client::Client::new(snap_wasm_browser::Http::new(
        authy::operations::http_routes(),
    ));
    let account = authy::client::account(&mut transport)
        .await
        .map_err(error)?;
    serde_json::to_string(&account).map_err(error)
}
