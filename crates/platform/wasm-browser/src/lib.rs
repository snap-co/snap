//! Browser HTTP IO for the Rust Transport SDK. Cookies remain browser-managed.
//! Each command is sent once; cancellation or IO failure never replays a mutation.
use snap_transport::{Channel, Command, Error, Event, Response, Value};
use std::collections::VecDeque;

pub struct Http {
    routes: Vec<snap_transport::carrier::HttpRoute>,
    /// Observations already fetched from the wire, waiting to be handed out one
    /// per `receive`. HTTP answers a request with the whole exchange, so the
    /// platform reassembles it into the single-event frames Transport carries.
    buffered: VecDeque<Response>,
}
impl Http {
    pub fn new(routes: impl IntoIterator<Item = snap_transport::carrier::HttpRoute>) -> Self {
        Self {
            routes: routes.into_iter().collect(),
            buffered: VecDeque::new(),
        }
    }
}

impl Channel for Http {
    /// Performs the round trip. Plain HTTP cannot pipeline, so acceptance cannot
    /// be published before the handler runs: a slow operation is dead air here.
    /// WebSocket carriers do not have this limit.
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        let Command::Request { invocation, .. } = command else {
            return Err(Error::Protocol);
        };
        let route = self
            .routes
            .iter()
            .find(|route| route.operation == invocation.operation)
            .ok_or(Error::UnknownOperation)?;
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
        // HTTP exposes only completion; a successful terminal result proves that
        // admission happened. Synthesize the acceptance Transport's ordering
        // contract requires, then hand out one event per receive.
        if outcome.is_ok() {
            self.buffered.push_back(Response::Event(Event::Accepted { id }));
        }
        self.buffered
            .push_back(Response::Event(Event::Completed { id, outcome }));
        Ok(())
    }

    /// Yields the next buffered observation, or `None` once the exchange is
    /// exhausted. A connectionless request has no logical connection, so it
    /// never receives a `Global` push.
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        Ok(self.buffered.pop_front())
    }
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
