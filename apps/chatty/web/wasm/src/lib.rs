//! Browser IO stays in JavaScript. This binding uses the real Document client,
//! shared thread mutations and wire correlation for snapshots and optimistic edits.
use snap_document::{
    ClientMessage, ServerMessage,
    client::{Client, Outcome},
    wire::Wire,
};
use wasm_bindgen::prelude::*;
fn error(value: impl std::fmt::Debug) -> JsValue {
    JsValue::from_str(&format!("{value:?}"))
}
#[wasm_bindgen]
pub struct ChattyClient {
    client: Client,
    wire: Wire,
    registry: snap_document::Registry,
}
#[wasm_bindgen]
impl ChattyClient {
    #[wasm_bindgen(constructor)]
    pub fn new(actor: String) -> Self {
        Self {
            client: Client::new(actor),
            wire: Wire::default(),
            registry: chatty::registry(),
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
    pub fn invoke(&mut self, operation: &str, input: &str) -> Result<String, JsValue> {
        let command = self
            .wire
            .invoke(operation, serde_json::from_str(input).map_err(error)?)
            .map_err(error)?;
        serde_json::to_string(&command).map_err(error)
    }
    pub fn rename(&mut self, id: &str, title: &str, effort: &str) -> Result<String, JsValue> {
        self.client
            .enqueue(
                &self.registry,
                id,
                "rename",
                serde_json::json!({"title":title,"effort":effort}),
            )
            .map_err(error)?;
        let send = self.submit(false)?;
        self.result(send, None)
    }
    pub fn receive(&mut self, text: &str) -> Result<String, JsValue> {
        let response: snap_transport::Response = serde_json::from_str(text).map_err(error)?;
        if let snap_transport::Response::Attached { resumed } = response {
            if resumed {
                self.client.begin_reconnect();
            } else {
                self.client
                    .handle(&self.registry, ServerMessage::Reset)
                    .map_err(error)?;
            }
            let send = self.submit(true)?;
            return self.result(send, None);
        }
        if let snap_transport::Response::Failed(failure) = response {
            return self.result(vec![], Some(format!("{failure:?}")));
        }
        if matches!(response, snap_transport::Response::Detached) {
            return self.result(vec![], None);
        }
        let mut manifest = false;
        let mut failure = None;
        for message in self.wire.receive(response).map_err(error)? {
            manifest |= matches!(message, ServerMessage::Reset);
            match self.client.handle(&self.registry, message).map_err(error)? {
                Outcome::NeedManifest { error, .. } => {
                    manifest = true;
                    failure = Some(format!("Synchronization diverged: {error:?}"));
                }
                Outcome::Rejected { error, .. } => {
                    failure = Some(format!("Edit rejected: {error:?}"))
                }
                Outcome::Forbidden { .. } => failure = Some("Document access ended".into()),
                Outcome::Reconciled {
                    outcomes,
                    replay_error,
                    ..
                } => {
                    for outcome in outcomes {
                        match outcome {
                            Outcome::Rejected { error, .. } => {
                                failure = Some(format!("Edit rejected: {error:?}"))
                            }
                            Outcome::Forbidden { .. } => {
                                failure = Some("Document access ended".into())
                            }
                            _ => {}
                        }
                    }
                    if failure.is_none() {
                        failure = replay_error.map(|error| format!("Replay failed: {error:?}"));
                    }
                }
                _ => {}
            }
        }
        let send = self.submit(manifest)?;
        self.result(send, failure)
    }
}
impl ChattyClient {
    fn submit(&mut self, manifest: bool) -> Result<Vec<String>, JsValue> {
        let message = if manifest {
            Some(ClientMessage::Manifest(self.client.manifest()))
        } else if !self.client.is_reconciling() && !self.client.needs_recovery() {
            self.client.next_submission()
        } else {
            None
        };
        message
            .map(|message| {
                self.wire
                    .submit(message)
                    .map_err(error)
                    .and_then(|command| serde_json::to_string(&command).map_err(error))
            })
            .transpose()
            .map(|frame| frame.into_iter().collect())
    }
    fn result(&self, send: Vec<String>, error: Option<String>) -> Result<String, JsValue> {
        // Revisions are rendered as strings rather than lossy JavaScript numbers.
        let documents:Vec<_>=self.client.view().values().map(|s|serde_json::json!({"id":s.id,"revision":s.revision.to_string(),"value":s.value})).collect();
        serde_json::to_string(&serde_json::json!({"documents":documents,"pending":self.client.pending().len(),"send":send,"error":error,"ready":!self.client.is_reconciling() && !self.client.needs_recovery()})).map_err(crate::error)
    }
}

#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn identity_fetch(origin: String) -> Result<String, wasm_bindgen::JsValue> {
    snap_react_bindings::oauth_fetch(&origin).await
}
