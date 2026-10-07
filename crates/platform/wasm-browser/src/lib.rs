//! Browser carrier selection for module SDKs. Ordinary calls use the runtime's
//! identified WebSocket connection; only declared exceptions use HTTP.
//! Selection happens before submission. No failure replays work on another carrier.
use snap_transport::{Error, Event, Invocation, Operation, Outcome, Value};

/// Connected invocation handoff supplied by the browser runtime. It uses the
/// same socket and invocation allocator as the application's replica binding.
pub type Invoke = js_sys::Function;

#[derive(Default)]
pub struct Client {
    http: Http,
    ids: snap_transport::client::InvocationIds,
    invoke: Option<Invoke>,
}
impl Client {
    pub fn connected(invoke: Invoke) -> Self {
        Self {
            invoke: Some(invoke),
            ..Self::default()
        }
    }
}
impl snap_transport::client::Operations for Client {
    async fn call<O: Operation>(&mut self, input: &O::Input) -> Result<O::Output, Error> {
        let input = serde_json::to_value(input).map_err(|_| Error::InvalidInput)?;
        let value = match O::http_route() {
            Some(route) => {
                self.http
                    .request(
                        route,
                        Invocation {
                            id: self.ids.allocate()?,
                            operation: O::NAME.into(),
                            input,
                        },
                    )
                    .await?
            }
            None => {
                connected(
                    self.invoke.as_ref().ok_or(Error::Unavailable)?,
                    O::NAME,
                    input,
                )
                .await?
            }
        };
        snap_transport::client::decode(value)
    }
}

/// Terminal HTTP IO. It has no connection, admission stream or reply buffer,
/// and does not implement `Channel`. Cookies remain browser-managed.
#[derive(Default)]
pub struct Http;
impl Http {
    pub async fn request(
        &mut self,
        route: snap_transport::carrier::HttpRoute,
        invocation: Invocation,
    ) -> Outcome {
        if route.operation != invocation.operation {
            return Err(Error::UnknownOperation);
        }
        let method = match route.method {
            snap_transport::carrier::HttpMethod::Get => "GET",
            snap_transport::carrier::HttpMethod::Post => "POST",
        };
        let path = format!("/{}", invocation.operation.replace('.', "/"));
        let input = (method == "POST").then(|| invocation.input.to_string());
        let value = request(&path, method, input.as_deref(), Some(invocation.id)).await?;
        let event: Event = serde_json::from_value(value).map_err(|_| Error::Protocol)?;
        let Event::Completed { id, outcome } = event else {
            return Err(Error::Protocol);
        };
        if id != invocation.id {
            return Err(Error::Protocol);
        }
        outcome
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn connected(_: &Invoke, _: &str, _: Value) -> Outcome {
    Err(Error::Unavailable)
}
#[cfg(target_arch = "wasm32")]
async fn connected(invoke: &Invoke, operation: &str, input: Value) -> Outcome {
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    let result = invoke
        .call2(
            &JsValue::NULL,
            &JsValue::from_str(operation),
            &JsValue::from_str(&input.to_string()),
        )
        .map_err(|_| Error::Unavailable)?;
    let result = JsFuture::from(
        result
            .dyn_into::<js_sys::Promise>()
            .map_err(|_| Error::Protocol)?,
    )
    .await
    .map_err(|error| {
        error
            .as_string()
            .and_then(|value| serde_json::from_str::<Error>(&value).ok())
            .unwrap_or(Error::Unavailable)
    })?;
    serde_json::from_str(&result.as_string().ok_or(Error::Protocol)?).map_err(|_| Error::Protocol)
}
pub async fn get_json(path: &str) -> Result<Value, Error> {
    request(path, "GET", None, None).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn request(_: &str, _: &str, _: Option<&str>, _: Option<u64>) -> Result<Value, Error> {
    Err(Error::Unavailable)
}
#[cfg(target_arch = "wasm32")]
async fn request(
    path: &str,
    method: &str,
    body: Option<&str>,
    id: Option<u64>,
) -> Result<Value, Error> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let options = web_sys::RequestInit::new();
    options.set_method(method);
    options.set_credentials(web_sys::RequestCredentials::SameOrigin);
    options.set_redirect(web_sys::RequestRedirect::Error);
    if let Some(body) = body {
        options.set_body(&wasm_bindgen::JsValue::from_str(body));
    }
    let request =
        web_sys::Request::new_with_str_and_init(path, &options).map_err(|_| Error::InvalidInput)?;
    request
        .headers()
        .set("accept", "application/json")
        .map_err(|_| Error::InvalidInput)?;
    if method == "POST" {
        request
            .headers()
            .set("content-type", "application/json")
            .map_err(|_| Error::InvalidInput)?;
    }
    if let Some(id) = id {
        request
            .headers()
            .set("x-snap-operation-id", &id.to_string())
            .map_err(|_| Error::InvalidInput)?;
    }
    let window = web_sys::window().ok_or(Error::Unavailable)?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| Error::Unavailable)?
        .dyn_into()
        .map_err(|_| Error::Protocol)?;
    let text = JsFuture::from(response.text().map_err(|_| Error::Protocol)?)
        .await
        .map_err(|_| Error::Unavailable)?
        .as_string()
        .ok_or(Error::Protocol)?;
    let value = serde_json::from_str(&text).map_err(|_| Error::Protocol)?;
    // Operation errors live in the typed completion even for non-2xx statuses.
    if id.is_none() && !response.ok() {
        return Err(Error::Unavailable);
    }
    Ok(value)
}
