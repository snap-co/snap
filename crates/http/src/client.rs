//! Host-owned outbound HTTP with bounded, incremental bodies. Applications select
//! destinations; this interface never follows a URL supplied by a remote token.
use alloc::{string::String, vec::Vec};
use core::future::Future;

pub struct Outgoing {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Maximum response bytes across the entire stream, enforced by the host.
    pub max_bytes: usize,
    /// Total deadline including connect, response headers and body consumption.
    pub timeout_ms: u64,
}
pub trait Body {
    fn chunk(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>>;
}
pub struct Incoming<B> {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: B,
}
/// Implementations must not follow redirects or retry requests automatically.
/// A timeout/disconnect after sending does not prove a remote mutation was undone.
pub trait Client: Clone + 'static {
    type Body: Body;
    fn send(&self, request: Outgoing)
    -> impl Future<Output = Result<Incoming<Self::Body>, String>>;
}
pub async fn collect(body: &mut impl Body, max: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.chunk().await? {
        if bytes.len() + chunk.len() > max {
            return Err("Remote response too large".into());
        }
        bytes.extend(chunk);
    }
    Ok(bytes)
}
