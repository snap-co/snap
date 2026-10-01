use snap_http::client::{Body, Client, Incoming, Outgoing};
use std::time::Duration;

#[derive(Clone)]
pub struct Http(reqwest::Client);
impl Http {
    pub fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map(Self)
            .map_err(|_| "Cannot initialize HTTP".into())
    }
}
pub struct Stream {
    response: reqwest::Response,
    remaining: usize,
}
impl Body for Stream {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some(bytes) = self
            .response
            .chunk()
            .await
            .map_err(|_| "Remote stream interrupted")?
        else {
            return Ok(None);
        };
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or("Remote response too large")?;
        Ok(Some(bytes.to_vec()))
    }
}
impl Client for Http {
    type Body = Stream;
    async fn send(&self, request: Outgoing) -> Result<Incoming<Self::Body>, String> {
        let method =
            reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| "Invalid method")?;
        let mut builder = self
            .0
            .request(method, request.url)
            .timeout(Duration::from_millis(request.timeout_ms));
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(|_| "Remote request failed; outcome may be unknown")?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
            .collect();
        Ok(Incoming {
            status,
            headers,
            body: Stream {
                response,
                remaining: request.max_bytes,
            },
        })
    }
}
