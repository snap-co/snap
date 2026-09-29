//! Read-only workspace replication uses the Document SDK. Domain commands go to
//! the same serialized application operations from browser and CLI.
use snap_document::{
    ClientMessage, ServerMessage,
    client::{Client, Outcome},
    wire::Wire,
};
use wasm_bindgen::prelude::*;
fn error(v: impl core::fmt::Debug) -> JsValue {
    JsValue::from_str(&format!("{v:?}"))
}
#[wasm_bindgen]
pub struct FactorioClient {
    client: Client,
    wire: Wire,
}
#[wasm_bindgen]
impl FactorioClient {
    pub fn invoke(&mut self, operation: &str, input: &str) -> Result<String, JsValue> {
        let command = self
            .wire
            .invoke(operation, serde_json::from_str(input).map_err(error)?)
            .map_err(error)?;
        serde_json::to_string(&command).map_err(error)
    }
    #[wasm_bindgen(constructor)]
    pub fn new(actor: String) -> Self {
        Self {
            client: Client::new(actor),
            wire: Wire::default(),
        }
    }
    pub fn connect(&mut self, id: &str) -> Result<String, JsValue> {
        self.wire.reconnect();
        serde_json::to_string(&snap_transport::Command::Connect {
            bearer: String::new(),
            client_id: id.into(),
        })
        .map_err(error)
    }
    pub fn receive(&mut self, text: &str) -> Result<String, JsValue> {
        let response: snap_transport::Response = serde_json::from_str(text).map_err(error)?;
        let registry = factorio::documents::registry();
        let mut manifest = false;
        match response {
            snap_transport::Response::Attached { resumed } => {
                if resumed {
                    self.client.begin_reconnect();
                } else {
                    self.client
                        .handle(&registry, ServerMessage::Reset)
                        .map_err(error)?;
                }
                manifest = true;
            }
            snap_transport::Response::Failed(failure) => return Err(error(failure)),
            snap_transport::Response::Detached => return Err(error("Disconnected")),
            response => {
                for message in self.wire.receive(response).map_err(error)? {
                    manifest |= matches!(
                        self.client.handle(&registry, message).map_err(error)?,
                        Outcome::NeedManifest { .. }
                    );
                }
            }
        }
        let send = if manifest {
            vec![
                serde_json::to_string(
                    &self
                        .wire
                        .submit(ClientMessage::Manifest(self.client.manifest()))
                        .map_err(error)?,
                )
                .map_err(error)?,
            ]
        } else {
            vec![]
        };
        serde_json::to_string(&serde_json::json!({"documents":self.client.view(),"send":send,"ready":!self.client.is_reconciling() && !self.client.needs_recovery()}))
            .map_err(error)
    }
}
