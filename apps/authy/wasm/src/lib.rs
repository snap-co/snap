//! Authy browser binding over the shared Document client SDK.
//!
//! JavaScript owns actual WebSocket IO, reconnect timers and forms. Rust owns
//! profile mutation behavior, optimistic reconciliation and wire correlation
//! through [`snap_document::client::Client`], [`snap_document::wire::Wire`]
//! and [`authy::registry`]. There is no JS duplicate of mutation or
//! reconciliation logic.
//!
//! Wire protocol (served by the coordinator-owned host):
//!
//! * HTTP `POST /api/signup`, `POST /api/login` with `{email,password}` and
//!   `POST /api/logout` with `{}`; `GET /api/session` returns
//!   `{account:null|{identity,email,profile,authenticated_at}}`. The
//!   `HttpOnly` same-origin cookie is the auth; no bearer is exposed to JS.
//! * Socket `/transport` carries standard `snap_transport` `Command`/`Response`
//!   JSON text frames. The browser sends `Connect {bearer:"", client_id}`; the
//!   host fills the bearer from the upgrade cookie.
//!
//! Non-obvious guarantees beside this interface:
//!
//! * `Connect` commands and every `Invoke` leave through [`Wire`], so sender
//!   wire invocation IDs are managed here, never in JS.
//! * `Attached {resumed:true}` enters the surviving-reconnect gate
//!   (`begin_reconnect`) and answers with a `Manifest` of current
//!   holdings/pending. `Attached {resumed:false}` first applies a logical
//!   `Reset` (clearing expired journal/pending) and then answers with an empty
//!   manifest. A new server process therefore clears old pending and reloads
//!   the profile from the fresh manifest; a surviving reconnect retains the
//!   journal for receipt deduplication.
//! * Every `Events`/`Notification` frame goes through [`Wire::receive`] and
//!   then [`Client::handle`] with the shared registry. `Holdings` pushes stay
//!   distinct from correlated `Manifest` results; wire correlation decides.
//! * A `NeedManifest` outcome is an observable `error` plus one correlated
//!   `Manifest` command in `send`. `next_submission` paces mutation sends by
//!   acceptance ACK; `enqueue_edit` projects immediately so the UI updates
//!   before the ACK. Profile `edit` intents carry `{name,bio,revision}` where
//!   `revision` is the projected base revision read here in Rust (never a JS
//!   number), keeping causal ACK-paced edits guarded without a blanket
//!   stale-base policy in shared Document code.
//! * Methods return JSON text so 64-bit revisions never cross JS numbers as
//!   values; JS parses only for display and always sends back the exact frame
//!   text Rust produced.

use snap_document::{ServerMessage, client::Client, wire::Wire};
use wasm_bindgen::prelude::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_preserves_large_revisions_and_recovered_rejections() {
        let id = "00000000-0000-4000-8000-000000000001";
        let mut client = Client::new("alice".into());
        client
            .handle(
                &authy::registry(),
                ServerMessage::Manifest(snap_document::Reconciliation {
                    documents: vec![snap_document::Snapshot {
                        id: id.into(),
                        kind: authy::PROFILE_KIND.into(),
                        version: "1".into(),
                        revision: 9007199254740993,
                        value: serde_json::json!({"name":"Alice","bio":""}),
                    }],
                    completed: vec![],
                }),
            )
            .unwrap();
        let value = snapshot_value(&client, id);
        assert_eq!(value["profile"]["revision"], "9007199254740993");
        assert_eq!(value["authoritative"]["revision"], "9007199254740993");
        assert!(value["revision"].is_string());
        let outcome = snap_document::client::Outcome::Reconciled {
            documents: 1,
            completed: 1,
            outcomes: vec![snap_document::client::Outcome::Rejected {
                id: 1,
                error: snap_document::Error::Rejected("changed".into()),
            }],
            replay_error: None,
        };
        assert!(outcome_error(&outcome).unwrap().contains("edit rejected"));
    }
}

/// Browser-owned profile sync for one signed-in account.
///
/// `actor` is the HTTP session account `identity`; `profile` is the account
/// `profile` document id. Recreate this object when the account changes
/// (login/logout/different identity). Keep it across socket reconnects so a
/// surviving logical connection can retain its journal.
#[wasm_bindgen]
pub struct AuthyClient {
    client: Client,
    wire: Wire,
    registry: snap_document::Registry,
    profile: String,
}

fn js_error(message: String) -> JsValue {
    JsValue::from_str(&message)
}

fn snapshot_value(client: &Client, profile: &str) -> serde_json::Value {
    let project = |snapshot: &snap_document::Snapshot| {
        serde_json::json!({
            "id":snapshot.id,"kind":snapshot.kind,"version":snapshot.version,
            "revision":snapshot.revision.to_string(),"value":snapshot.value,
        })
    };
    let projected = client.get(profile).map(project);
    let authoritative = client.authoritative().get(profile).map(project);
    serde_json::json!({
        "actor": client.actor(),
        "profile_id": profile,
        "profile": projected,
        "authoritative": authoritative,
        "pending": client.pending().len(),
        "awaiting_ack": client.awaiting_ack().map(|id|id.to_string()),
        "reconciling": client.is_reconciling(),
        "needs_recovery": client.needs_recovery(),
        "revision": client.revision().to_string(),
    })
}

fn result_value(
    client: &Client,
    profile: &str,
    send: Vec<String>,
    error: Option<String>,
) -> Result<String, JsValue> {
    serde_json::to_string(&serde_json::json!({
        "snapshot": snapshot_value(client, profile),
        "send": send,
    "error": error,
    }))
    .map_err(|e| js_error(e.to_string()))
}

fn submit_manifest(
    client: &Client,
    wire: &mut Wire,
) -> Result<snap_transport::Command, snap_transport::Error> {
    wire.submit(snap_document::ClientMessage::Manifest(client.manifest()))
}

fn command_text(command: &snap_transport::Command) -> Result<String, snap_transport::Error> {
    serde_json::to_string(command).map_err(|_| snap_transport::Error::InvalidInput)
}

fn outcome_error(outcome: &snap_document::client::Outcome) -> Option<String> {
    use snap_document::client::Outcome;
    match outcome {
        Outcome::NeedManifest { error, .. } => Some(format!("NeedManifest:{error:?}")),
        // Surface mutation rejections with the wording the UI keys off so a
        // concurrent edit can be retried against the current view.
        Outcome::Rejected { error, .. } => Some(format!(
            "edit rejected: profile changed ({error:?}); retry against the current view"
        )),
        Outcome::Forbidden { document, .. } => Some(format!(
            "edit rejected: profile {document} is no longer readable; reloaded the current view"
        )),
        Outcome::Reconciled {
            outcomes,
            replay_error,
            ..
        } => outcomes.iter().find_map(outcome_error).or_else(|| {
            replay_error
                .as_ref()
                .map(|error| format!("Profile replay failed: {error:?}"))
        }),
        _ => None,
    }
}

#[wasm_bindgen]
impl AuthyClient {
    /// Create a sync client for the current HTTP session account.
    #[wasm_bindgen(constructor)]
    pub fn new(actor: String, profile: String) -> Result<AuthyClient, JsValue> {
        if actor.is_empty() || profile.is_empty() {
            return Err(js_error("actor and profile are required".into()));
        }
        Ok(Self {
            client: Client::new(actor),
            wire: Wire::default(),
            registry: authy::registry(),
            profile,
        })
    }

    /// Verified actor identity (HTTP account `identity`).
    pub fn actor(&self) -> String {
        self.client.actor().to_owned()
    }

    /// Profile document id (HTTP account `profile`).
    pub fn profile_id(&self) -> String {
        self.profile.clone()
    }

    /// Current projected snapshot as JSON text.
    pub fn snapshot(&self) -> Result<String, JsValue> {
        serde_json::to_string(&snapshot_value(&self.client, &self.profile))
            .map_err(|e| js_error(e.to_string()))
    }

    /// Build the `Connect {bearer:"", client_id}` command for JS to send.
    ///
    /// Resets the physical wire driver: stable intent IDs survive in the
    /// client journal, while physical invocation IDs restart per socket.
    /// `client_id` must stay stable within the tab across reconnects.
    pub fn connect_command(&mut self, client_id: &str) -> Result<String, JsValue> {
        if client_id.is_empty() {
            return Err(js_error("client_id is required".into()));
        }
        self.wire = Wire::default();
        let command = snap_transport::Command::Connect {
            bearer: String::new(),
            client_id: client_id.to_owned(),
        };
        command_text(&command).map_err(|e| js_error(format!("{e:?}")))
    }

    /// Handle `Attached {resumed}` and answer with the correlated `Manifest`.
    ///
    /// `resumed:true` retains the journal (`begin_reconnect`); `resumed:false`
    /// first clears expired state (`Reset`) so old pending never replays, then
    /// presents the fresh (empty) manifest.
    pub fn attached(&mut self, resumed: bool) -> Result<String, JsValue> {
        if resumed {
            self.client.begin_reconnect();
        } else {
            let registry = std::mem::replace(&mut self.registry, authy::registry());
            let _ = self.client.handle(&registry, ServerMessage::Reset);
            self.registry = registry;
        }
        let command = submit_manifest(&self.client, &mut self.wire)
            .map_err(|e| js_error(format!("{e:?}")))?;
        let text = command_text(&command).map_err(|e| js_error(format!("{e:?}")))?;
        result_value(&self.client, &self.profile, vec![text], None)
    }

    /// Enqueue the profile `edit` mutation with `{name,bio,revision}` args.
    ///
    /// `revision` is the projected base revision read from
    /// [`Client::get`] here in Rust before [`Client::enqueue`], never a JS
    /// number. This keeps causal ACK-paced edits guarded against concurrent
    /// profile changes without adding a blanket stale-base policy to shared
    /// Document code. Projects immediately (the returned snapshot already
    /// shows the edit) and, when the ACK gate is open, returns the next
    /// `Mutate` command to send. A concurrent-edit rejection arrives later as
    /// an `edit rejected: profile changed` error from [`receive`]; the UI
    /// keeps the current view so the user can retry.
    pub fn enqueue_edit(&mut self, name: &str, bio: &str) -> Result<String, JsValue> {
        let profile = self.profile.clone();
        let Some(base) = self.client.get(&profile).cloned() else {
            return result_value(
                &self.client,
                &self.profile,
                Vec::new(),
                Some("NotFound: profile not loaded yet; wait for the manifest".into()),
            );
        };
        let args = serde_json::json!({"name": name, "bio": bio, "revision": base.revision});
        let enqueue = {
            let registry = std::mem::replace(&mut self.registry, authy::registry());
            let result = self
                .client
                .enqueue(&registry, &profile, authy::PROFILE_MUTATION, args);
            self.registry = registry;
            result
        };
        let mut error: Option<String> = None;
        if let Err(e) = enqueue {
            error = Some(format!("{e:?}"));
        }
        let mut send = Vec::new();
        if error.is_none()
            && !self.client.is_reconciling()
            && !self.client.needs_recovery()
            && let Some(message) = self.client.next_submission()
        {
            match self.wire.submit(message) {
                Ok(command) => match command_text(&command) {
                    Ok(text) => send.push(text),
                    Err(e) => error = Some(format!("{e:?}")),
                },
                Err(e) => error = Some(format!("{e:?}")),
            }
        }
        result_value(&self.client, &self.profile, send, error)
    }

    /// Handle one incoming `/transport` response frame (JSON text).
    ///
    /// Feeds `Events`/`Notification` frames through the wire correlator and
    /// the shared client, sends at most one correlated `Manifest` on
    /// `NeedManifest`/`Reset`, otherwise paces one `Mutate` via
    /// `next_submission`. Returns `{snapshot, send, error}` as JSON text.
    pub fn receive(&mut self, frame: &str) -> Result<String, JsValue> {
        let response: snap_transport::Response =
            serde_json::from_str(frame).map_err(|e| js_error(format!("Protocol:{e}")))?;
        // `Attached` arriving here (instead of via `attached`) is handled on
        // the same path so JS routing mistakes cannot desync the journal.
        if let snap_transport::Response::Attached { resumed } = response {
            return self.attached(resumed);
        }
        if matches!(response, snap_transport::Response::Detached) {
            return result_value(&self.client, &self.profile, Vec::new(), None);
        }
        if let snap_transport::Response::Failed(error) = &response {
            return result_value(
                &self.client,
                &self.profile,
                Vec::new(),
                Some(format!("{error:?}")),
            );
        }
        let messages = self
            .wire
            .receive(response)
            .map_err(|e| js_error(format!("{e:?}")))?;
        let mut error: Option<String> = None;
        let mut needs_manifest = false;
        for message in messages {
            let is_reset = matches!(message, ServerMessage::Reset);
            let registry = std::mem::replace(&mut self.registry, authy::registry());
            let outcome = self.client.handle(&registry, message);
            self.registry = registry;
            match outcome {
                Ok(outcome) => {
                    if matches!(outcome, snap_document::client::Outcome::NeedManifest { .. }) {
                        needs_manifest = true;
                    }
                    if is_reset {
                        needs_manifest = true;
                    }
                    if error.is_none() {
                        error = outcome_error(&outcome);
                    }
                }
                Err(e) => {
                    if error.is_none() {
                        error = Some(format!("{e:?}"));
                    }
                }
            }
        }
        let mut send = Vec::new();
        if needs_manifest {
            match submit_manifest(&self.client, &mut self.wire) {
                Ok(command) => match command_text(&command) {
                    Ok(text) => send.push(text),
                    Err(e) => {
                        if error.is_none() {
                            error = Some(format!("{e:?}"));
                        }
                    }
                },
                Err(e) => {
                    if error.is_none() {
                        error = Some(format!("{e:?}"));
                    }
                }
            }
        } else if !self.client.is_reconciling()
            && !self.client.needs_recovery()
            && let Some(message) = self.client.next_submission()
        {
            match self.wire.submit(message) {
                Ok(command) => match command_text(&command) {
                    Ok(text) => send.push(text),
                    Err(e) => {
                        if error.is_none() {
                            error = Some(format!("{e:?}"));
                        }
                    }
                },
                Err(e) => {
                    if error.is_none() {
                        error = Some(format!("{e:?}"));
                    }
                }
            }
        }
        result_value(&self.client, &self.profile, send, error)
    }
}
