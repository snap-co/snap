//! Bounded fetch streams with explicit abort/deadline handling. Authy calls can use
//! a service binding while retaining the configured public issuer in the protocol.
use futures_util::{
    StreamExt,
    future::{Either, select},
};
use snap_http::client::{Body, Client, Incoming, Outgoing};
use std::time::Duration;
use worker::{
    AbortController, ByteStream, Delay, Fetch, Fetcher, Request, RequestInit, RequestRedirect,
};
#[derive(Clone, Default)]
pub struct Http {
    authy: Option<(String, Fetcher)>,
}
impl Http {
    pub fn with_authy(origin: String, binding: Fetcher) -> Self {
        Self {
            authy: Some((origin, binding)),
        }
    }
}
struct Abort(Option<AbortController>);
impl Drop for Abort {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.abort();
        }
    }
}
pub struct ResponseBody {
    stream: Option<ByteStream>,
    remaining: usize,
    deadline: u64,
    _abort: Abort,
}
impl Client for Http {
    type Body = ResponseBody;
    async fn send(&self, request: Outgoing) -> Result<Incoming<Self::Body>, String> {
        let deadline = crate::crypto::now().saturating_add(request.timeout_ms);
        let method = match request.method {
            "GET" => worker::Method::Get,
            "POST" => worker::Method::Post,
            _ => return Err("Unsupported HTTP method".into()),
        };
        let mut init = RequestInit::new();
        init.with_method(method)
            .with_redirect(RequestRedirect::Manual);
        for (name, value) in request.headers {
            init.headers
                .append(&name, &value)
                .map_err(|_| "Invalid outbound header")?;
        }
        if !request.body.is_empty() {
            init.with_body(Some(
                js_sys::Uint8Array::from(request.body.as_slice()).into(),
            ));
        }
        let req =
            Request::new_with_init(&request.url, &init).map_err(|_| "Invalid outbound request")?;
        let controller = AbortController::default();
        let signal = controller.signal();
        let abort = Abort(Some(controller));
        let fetch = async {
            if let Some((origin, binding)) = &self.authy
                && request.url.starts_with(&format!("{origin}/"))
            {
                binding.fetch_request(req).await
            } else {
                Fetch::Request(req).send_with_signal(&signal).await
            }
        };
        let mut response = match select(
            Box::pin(fetch),
            Box::pin(Delay::from(Duration::from_millis(request.timeout_ms))),
        )
        .await
        {
            Either::Left((Ok(response), _)) => response,
            Either::Left((Err(_), _)) => return Err("Remote connection failed".into()),
            _ => return Err("Remote request timed out".into()),
        };
        let status = response.status_code();
        let headers = response.headers().entries().collect();
        Ok(Incoming {
            status,
            headers,
            body: ResponseBody {
                stream: response.stream().ok(),
                remaining: request.max_bytes,
                deadline,
                _abort: abort,
            },
        })
    }
}
impl Body for ResponseBody {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some(stream) = &mut self.stream else {
            return Ok(None);
        };
        let remaining = self.deadline.saturating_sub(crate::crypto::now());
        if remaining == 0 {
            return Err("Remote body timed out".into());
        }
        match select(
            Box::pin(stream.next()),
            Box::pin(Delay::from(Duration::from_millis(remaining))),
        )
        .await
        {
            Either::Left((Some(Ok(bytes)), _)) => {
                if bytes.len() > self.remaining {
                    return Err("Remote response too large".into());
                }
                self.remaining -= bytes.len();
                Ok(Some(bytes))
            }
            Either::Left((None, _)) => Ok(None),
            Either::Left((Some(Err(_)), _)) => Err("Remote stream interrupted".into()),
            _ => Err("Remote body timed out".into()),
        }
    }
}
