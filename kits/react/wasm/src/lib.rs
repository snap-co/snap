//! React/Wasm glue over Rust module SDKs. No credential/session behavior lives
//! in JavaScript; the browser transport handles HTTP and browser-managed cookies.
use snap_wasm_browser::Client;
pub use snap_wasm_browser::Invoke;
use wasm_bindgen::prelude::*;
fn error(error: snap_transport::Error) -> JsValue {
    JsValue::from_str(snap_identity::client::message(&error))
}

pub async fn fetch() -> Result<String, JsValue> {
    let mut transport = Client::default();
    let value = snap_identity::client::Client::new(&mut transport)
        .fetch()
        .await
        .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid identity output"))
}
pub async fn acquire(email: &str, password: &str, enroll: bool) -> Result<String, JsValue> {
    let mut transport = Client::default();
    let mut client = snap_identity::client::Client::new(&mut transport);
    let value = if enroll {
        client.enroll(email, password).await
    } else {
        client.acquire(email, password).await
    }
    .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid identity output"))
}
pub async fn release(scope: &str) -> Result<(), JsValue> {
    let scope = snap_identity::ReleaseScope::parse(scope)
        .map_err(|_| JsValue::from_str("Invalid release scope"))?;
    let mut transport = Client::default();
    snap_identity::client::Client::new(&mut transport)
        .release(scope)
        .await
        .map_err(error)
}
pub async fn sessions(invoke: Invoke) -> Result<String, JsValue> {
    let mut transport = Client::connected(invoke);
    let value = snap_identity::client::Client::new(&mut transport)
        .sessions()
        .await
        .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid identity output"))
}
pub async fn credentials(invoke: Invoke) -> Result<String, JsValue> {
    let mut transport = Client::connected(invoke);
    let value = snap_identity::client::Client::new(&mut transport)
        .credentials()
        .await
        .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid identity output"))
}
pub async fn passkey_begin_registration(label: &str, binding: &str) -> Result<String, JsValue> {
    let mut transport = Client::default();
    let value = snap_identity::client::Client::new(&mut transport)
        .begin_passkey_registration(label, binding)
        .await
        .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid passkey challenge"))
}
pub async fn passkey_begin_authentication(
    locator: Option<&str>,
    name: Option<&str>,
    binding: &str,
) -> Result<String, JsValue> {
    let mut transport = Client::default();
    let mut client = snap_identity::client::Client::new(&mut transport);
    let value = match name {
        Some(name) => {
            client
                .begin_named_passkey_authentication(name, binding)
                .await
        }
        None => client.begin_passkey_authentication(locator, binding).await,
    }
    .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid passkey challenge"))
}
pub async fn passkey_finish(proof: &str, registration: bool) -> Result<String, JsValue> {
    let proof =
        serde_json::from_str(proof).map_err(|_| JsValue::from_str("Invalid passkey response"))?;
    let mut transport = Client::default();
    let mut client = snap_identity::client::Client::new(&mut transport);
    let value = if registration {
        client.finish_passkey_registration(&proof).await
    } else {
        client.finish_passkey_authentication(&proof).await
    }
    .map_err(error)?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid identity output"))
}
pub async fn oauth_fetch(origin: &str) -> Result<String, JsValue> {
    let value = snap_wasm_browser::get_json(&format!("{origin}/api/session"))
        .await
        .map_err(error)?;
    let value = snap_oidc::client::project(value)
        .map_err(|_| JsValue::from_str("Invalid identity projection"))?;
    serde_json::to_string(&value).map_err(|_| JsValue::from_str("Invalid identity output"))
}

/// Application Wasm entrypoints export these bindings without defining Identity
/// operation names, wire types or policies themselves.
#[macro_export]
macro_rules! export_identity {
    () => {
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_fetch() -> Result<String, wasm_bindgen::JsValue> {
            $crate::fetch().await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_enroll(
            email: String,
            password: String,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::acquire(&email, &password, true).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_acquire(
            email: String,
            password: String,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::acquire(&email, &password, false).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_release(scope: String) -> Result<(), wasm_bindgen::JsValue> {
            $crate::release(&scope).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_sessions(
            invoke: $crate::Invoke,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::sessions(invoke).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_credentials(
            invoke: $crate::Invoke,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::credentials(invoke).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_passkey_register(
            label: String,
            binding: String,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::passkey_begin_registration(&label, &binding).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_passkey_authenticate(
            locator: Option<String>,
            name: Option<String>,
            binding: String,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::passkey_begin_authentication(locator.as_deref(), name.as_deref(), &binding)
                .await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_passkey_registered(
            proof: String,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::passkey_finish(&proof, true).await
        }
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub async fn identity_passkey_authenticated(
            proof: String,
        ) -> Result<String, wasm_bindgen::JsValue> {
            $crate::passkey_finish(&proof, false).await
        }
    };
}
