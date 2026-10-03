//! Browser binding for the same Testy SDK used by memory and native clients.
//! JavaScript owns socket lifetime. The channel is a stream: `send` writes one
//! command and returns, `receive` yields the next frame whenever it arrives, so
//! acceptance and progress reach the client while the operation is still running.
use snap_transport::{Channel, Command, Error, Response, json};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    pub type BrowserChannel;
    #[wasm_bindgen(method, catch)]
    async fn send(this: &BrowserChannel, command: String) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    async fn receive(this: &BrowserChannel) -> Result<JsValue, JsValue>;
}
struct Connection(BrowserChannel);
impl Channel for Connection {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        let encoded = serde_json::to_string(&command).map_err(|_| Error::Protocol)?;
        self.0.send(encoded).await.map_err(|_| Error::Unavailable)
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        let value = self.0.receive().await.map_err(|_| Error::Unavailable)?;
        let frame = value.as_string().ok_or(Error::Protocol)?;
        serde_json::from_str(&frame).map_err(|_| Error::Protocol)
    }
}
fn error(error: Error) -> JsValue {
    JsValue::from_str(&format!("{error:?}"))
}

/// Keep dependency JSON and observation IDs out of JavaScript's numeric type.
/// The browser passes editable JSON as text; Rust validates and embeds its value.
#[wasm_bindgen]
pub fn development_control(action: &str, input: Option<String>) -> Result<String, JsValue> {
    let parse = |text: &str| {
        serde_json::from_str::<serde_json::Value>(text)
            .map_err(|error| JsValue::from_str(&error.to_string()))
    };
    let mut action = parse(action)?;
    if let Some(ticket) = action.get("ticket").and_then(serde_json::Value::as_str) {
        let ticket = ticket
            .parse::<u64>()
            .map_err(|_| JsValue::from_str("Invalid ticket"))?;
        action["ticket"] = json!(ticket);
    }
    if let Some(input) = input {
        action["value"] = parse(&input)?;
    }
    Ok(action.to_string())
}

/// Structured UI flags plus preformatted records/trace. Pretty-print in Rust so
/// every JSON integer retains its exact value and numeric representation on screen.
#[wasm_bindgen]
pub fn development_observation(encoded: &str) -> Result<String, JsValue> {
    let mut value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|error| JsValue::from_str(&error.to_string()))?;
    // HTTP reports and debugger state/result envelopes share the exact formatter.
    let report = match value.get("type").and_then(serde_json::Value::as_str) {
        Some("state") => "state",
        Some("result") => "result",
        _ => "",
    };
    if report.is_empty() {
        format_observation(&mut value);
    } else if let Some(report) = value.get_mut(report) {
        format_observation(report);
    }
    Ok(value.to_string())
}

fn format_observation(value: &mut serde_json::Value) {
    for key in ["states", "trace"] {
        if let Some(records) = value.get_mut(key) {
            *records = json!(serde_json::to_string_pretty(records).unwrap());
        }
    }
    if let Some(ticket) = value
        .get_mut("active")
        .and_then(|active| active.get_mut("ticket"))
    {
        *ticket = json!(ticket.to_string());
    }
}

#[wasm_bindgen]
pub struct Client {
    inner: testy::Client<Connection>,
}
#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(channel: BrowserChannel) -> Self {
        Self {
            inner: testy::Client::new(Connection(channel)),
        }
    }
    pub async fn start(&mut self, id: &str) -> Result<(), JsValue> {
        self.inner.start(id).await.map_err(error)
    }
    pub fn use_session(&mut self, bearer: &str) -> Result<(), JsValue> {
        self.inner.use_session(bearer).map_err(error)
    }
    pub async fn authenticate(
        &mut self,
        enroll: bool,
        email: &str,
        password: &str,
    ) -> Result<String, JsValue> {
        self.inner
            .authenticate(enroll, email, password)
            .await
            .map_err(error)
    }
    pub async fn logout(&mut self) -> Result<(), JsValue> {
        self.inner.logout().await.map_err(error)
    }
    pub async fn disconnect(&mut self) -> Result<(), JsValue> {
        self.inner.disconnect().await.map_err(error)
    }
    pub async fn close(&mut self) -> Result<(), JsValue> {
        self.inner.close().await.map_err(error)
    }
    pub async fn health(&mut self) -> Result<String, JsValue> {
        Ok(self.inner.health().await.map_err(error)?.to_string())
    }
    pub async fn calculate(&mut self, operation: &str, operand: &str) -> Result<String, JsValue> {
        let operand = operand
            .parse::<i64>()
            .map_err(|_| JsValue::from_str("Enter a signed 64-bit integer"))?;
        let result = match operation {
            "add" => self.inner.add(operand).await,
            "sub" => self.inner.sub(operand).await,
            "mul" => self.inner.mul(operand).await,
            "div" => self.inner.div(operand).await,
            "add_checked" => self.inner.add_checked(operand).await,
            _ => Err(Error::UnknownOperation),
        }
        .map_err(error)?;
        Ok(result.to_string())
    }
    /// Decimal strings preserve all signed-64-bit values across JavaScript.
    pub async fn inspect(&mut self) -> Result<String, JsValue> {
        let calc = self.inner.inspect().await.map_err(error)?;
        Ok(json!({"accumulator": calc.accumulator.to_string(), "history": calc.history.iter().map(|entry|
            json!({"operation": entry.operation, "operand": entry.operand.to_string(), "before": entry.before.to_string(), "after": entry.after.to_string()})
        ).collect::<Vec<_>>()}).to_string())
    }
}
