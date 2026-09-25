//! Chatty's portable account/session and conversation behavior. Hosts supply all
//! network, clock, randomness, task lifetime, signature verification and file IO.
#![no_std]
extern crate alloc;
pub mod session;
pub mod storage;
pub mod threads;
pub mod tools;
pub mod web;
use alloc::{string::String, vec::Vec};
use core::future::Future;
use serde_json::Value;
use snap_http::{FutureValue, Response, client::Client};

#[derive(Clone)]
pub struct Config {
    pub origin: String,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub model: snap_llm::Config,
    pub exa_key: String,
    pub files: bool,
}
#[derive(Clone)]
pub enum FileRequest {
    List,
    Read { path: String },
    Write { path: String, content: String },
}
pub trait Host: Client {
    fn now(&self) -> u64;
    fn random(&self) -> Result<String, Response>;
    fn verify(&self, token: String, jwks: Value) -> impl Future<Output = Result<Value, Response>>;
    /// Accepted generation futures are retained after their HTTP observer leaves.
    /// Runtime loss interrupts them; restart marks unfinished turns interrupted.
    fn spawn(&self, future: FutureValue<()>);
    fn sleep(&self, milliseconds: u64) -> impl Future<Output = ()>;
    fn files(
        &self,
        owner: String,
        request: FileRequest,
    ) -> impl Future<Output = Result<Value, String>>;
}
pub fn schemas() -> Vec<snap_store::Schema> {
    storage::schemas()
}
