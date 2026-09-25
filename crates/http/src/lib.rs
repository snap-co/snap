//! Owned HTTP messages for application-selected standards endpoints and host IO.
//! This carrier has no Snap Build negotiation or completion envelope. Applications
//! own method, content type, origin and authorization policy for their routes.
#![no_std]
extern crate alloc;
use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{future::Future, pin::Pin};

pub type FutureValue<T> = Pin<Box<dyn Future<Output = T> + 'static>>;
#[derive(Clone, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Lowercase field names. Hosts preserve duplicate values as separate entries.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Trusted host clock, Unix milliseconds. Never populated from a client field.
    pub now: u64,
}
impl Request {
    /// Security-sensitive singleton headers reject duplicates rather than choosing one.
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut values = self
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name));
        let value = values.next()?.1.as_str();
        values.next().is_none().then_some(value)
    }
    pub fn form(&self) -> Result<BTreeMap<String, String>, Response> {
        if self.method == "POST"
            && self.header("content-type").is_none_or(|h| {
                h.split(';').next().unwrap_or("").trim() != "application/x-www-form-urlencoded"
            })
        {
            return Err(Response::error(
                415,
                "invalid_request",
                "Expected a form body",
            ));
        }
        fields(if self.method == "GET" {
            self.query.as_bytes()
        } else {
            &self.body
        })
    }
}
pub fn fields(bytes: &[u8]) -> Result<BTreeMap<String, String>, Response> {
    let mut result = BTreeMap::new();
    for (k, v) in form_urlencoded::parse(bytes) {
        if result.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(Response::error(
                400,
                "invalid_request",
                "Duplicate parameter",
            ));
        }
    }
    Ok(result)
}
pub fn query(fields: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish()
}
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
#[derive(Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Response {
    pub fn new(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![
                ("content-type".into(), content_type.into()),
                ("cache-control".into(), "no-store".into()),
                ("pragma".into(), "no-cache".into()),
                ("x-content-type-options".into(), "nosniff".into()),
                ("referrer-policy".into(), "no-referrer".into()),
            ],
            body,
        }
    }
    pub fn json(status: u16, value: serde_json::Value) -> Self {
        Self::new(status, "application/json", value.to_string().into_bytes())
    }
    pub fn html(body: String) -> Self {
        Self::new(200, "text/html; charset=utf-8", body.into_bytes())
            .with("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'")
    }
    pub fn error(status: u16, code: &str, description: &str) -> Self {
        Self::json(
            status,
            serde_json::json!({"error":code,"error_description":description}),
        )
    }
    pub fn redirect(url: &str) -> Self {
        Self::new(303, "text/plain", Vec::new()).with("location", url)
    }
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}
/// A host retains an admitted call independently of the requesting connection.
/// Runtime loss can interrupt it; no automatic replay or durable execution is
/// promised. Implementations return owned futures and may be thread-local.
pub trait Service: 'static {
    fn routes(&self) -> &'static [&'static str];
    fn call(&self, request: Request) -> FutureValue<Response>;
}
/// Application-selected signed-cookie codec. A valid signature only identifies a
/// bearer candidate; the account module must still resolve current authority.
pub trait Cookie: Clone + 'static {
    fn read(&self, header: Option<&str>) -> Result<Option<String>, Response>;
    fn encode(&self, token: Option<&str>) -> String;
}
