//! Bounded outbound HTTP. No redirects or automatic retries, and one deadline
//! covers both headers and incremental body reads.
use snap_http::client::{Body, Client, Incoming, Outgoing};
use std::time::Duration;
#[derive(Clone)]
pub struct Http(reqwest::Client);
impl Http {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .user_agent("chatty/0.1")
                .build()?,
        ))
    }
}
pub struct ResponseBody {
    response: reqwest::Response,
    remaining: usize,
    deadline: tokio::time::Instant,
}
impl Client for Http {
    type Body = ResponseBody;
    async fn send(&self, request: Outgoing) -> Result<Incoming<Self::Body>, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| "Invalid HTTP method")?;
        let mut builder = self.0.request(method, &request.url).body(request.body);
        for (key, value) in request.headers {
            builder = builder.header(key, value);
        }
        let response = tokio::time::timeout_at(deadline, builder.send())
            .await
            .map_err(|_| "Remote request timed out")?
            .map_err(|_| "Remote connection failed")?;
        if response
            .content_length()
            .is_some_and(|n| n > request.max_bytes as u64)
        {
            return Err("Remote response too large".into());
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().into(), v.into())))
            .collect();
        Ok(Incoming {
            status,
            headers,
            body: ResponseBody {
                response,
                remaining: request.max_bytes,
                deadline,
            },
        })
    }
}
impl Body for ResponseBody {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        let chunk = tokio::time::timeout_at(self.deadline, self.response.chunk())
            .await
            .map_err(|_| "Remote body timed out")?
            .map_err(|_| "Remote stream interrupted")?;
        if let Some(bytes) = chunk {
            if bytes.len() > self.remaining {
                return Err("Remote response too large".into());
            }
            self.remaining -= bytes.len();
            Ok(Some(bytes.to_vec()))
        } else {
            Ok(None)
        }
    }
}
