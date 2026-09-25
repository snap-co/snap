//! Browser binding for the same Testy SDK used by memory and native clients.
//! JavaScript owns socket lifetime. Each exchange returns complete ordered events.
use snap_transport::{Channel, Command, Error, Response, json};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    pub type BrowserChannel;
    #[wasm_bindgen(method, catch)]
    async fn exchange(this: &BrowserChannel, command: String) -> Result<JsValue, JsValue>;
}
struct Connection(BrowserChannel);
impl Channel for Connection {
    async fn exchange(&mut self, command: Command) -> Result<Response, Error> {
        let encoded = serde_json::to_string(&command).map_err(|_| Error::Protocol)?;
        let value = self
            .0
            .exchange(encoded)
            .await
            .map_err(|_| Error::Unavailable)?;
        serde_json::from_str(&value.as_string().ok_or(Error::Protocol)?)
            .map_err(|_| Error::Protocol)
    }
}
fn error(error: Error) -> JsValue {
    JsValue::from_str(&format!("{error:?}"))
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
