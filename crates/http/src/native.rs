//! Native implementation of the shared outbound contract. One absolute deadline
//! covers headers and incremental bodies. Redirects and automatic retries are off.
use crate::client::{self, Incoming, Outgoing};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use std::time::Duration;
use tokio::time::{Instant, timeout_at};

#[derive(Clone)]
pub struct Client(reqwest::Client);
impl Client {
    pub fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map(Self)
            .map_err(|e| e.to_string())
    }
}
pub struct Body {
    response: reqwest::Response,
    deadline: Instant,
    remaining: usize,
    failed: bool,
}
impl client::Client for Client {
    type Body = Body;
    async fn send(&self, request: Outgoing) -> Result<Incoming<Body>, String> {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(request.timeout_ms))
            .ok_or_else(|| String::from("invalid HTTP deadline"))?;
        let mut builder = self.0.request(
            reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|e| e.to_string())?,
            request.url,
        );
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = timeout_at(deadline, builder.body(request.body).send())
            .await
            .map_err(|_| String::from("HTTP deadline exceeded"))?
            .map_err(|_| String::from("HTTP request failed"))?;
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.to_string(),
                    v.to_str()
                        .map_err(|_| String::from("invalid HTTP header"))?
                        .into(),
                ))
            })
            .collect::<Result<_, String>>()?;
        Ok(Incoming {
            status: response.status().as_u16(),
            headers,
            body: Body {
                response,
                deadline,
                remaining: request.max_bytes,
                failed: false,
            },
        })
    }
}
impl client::Body for Body {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.failed {
            return Err("HTTP body unavailable".into());
        }
        self.failed = true;
        let chunk = timeout_at(self.deadline, self.response.chunk())
            .await
            .map_err(|_| String::from("HTTP deadline exceeded"))?
            .map_err(|_| String::from("HTTP body failed"))?;
        let chunk = match chunk {
            Some(bytes) => {
                self.remaining = self
                    .remaining
                    .checked_sub(bytes.len())
                    .ok_or_else(|| String::from("HTTP body limit exceeded"))?;
                Some(bytes.to_vec())
            }
            None => None,
        };
        self.failed = false;
        Ok(chunk)
    }
}
