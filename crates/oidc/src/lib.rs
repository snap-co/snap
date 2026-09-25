//! Authorization-code OIDC issuer. Applications register clients and supply current
//! account authority. Hosts own signing, randomness and HTTP execution. A single
//! Store transaction consumes each code/refresh token and grants its successors.
#![no_std]
extern crate alloc;
pub mod issuer;
pub mod storage;
use alloc::{string::String, vec::Vec};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use core::future::Future;
use serde_json::Value;
use sha2::{Digest, Sha256};
use snap_http::Response;
use snap_store::Guard;
use subtle::ConstantTimeEq;

pub use issuer::{Client, Issuer, ROUTES};

/// Host-generated random values must contain at least 256 unpredictable bits,
/// encoded without padding as base64url. Sign and verify are pinned to RS256;
/// implementations must never accept an algorithm selected solely by token input.
pub trait Crypto: Clone + 'static {
    fn random(&self) -> Result<String, Response>;
    fn jwks(&self) -> Value;
    fn sign(&self, claims: Value) -> impl Future<Output = Result<String, Response>>;
    fn verify(&self, token: String) -> impl Future<Output = Result<Value, Response>>;
}
#[derive(Clone)]
pub struct Identity {
    pub subject: String,
    pub session: String,
    pub auth_time: u64,
    pub claims: Value,
    /// Module-owned current-session guards, checked with dependent issuer writes.
    pub authority: Vec<Guard>,
}
/// Issuer grants remain tied to the originating authentication session. Loading
/// claims must check current account/session authority; cached claims are advisory.
pub trait Accounts: Clone + 'static {
    fn load(
        &self,
        subject: String,
        session: String,
        now: u64,
    ) -> impl Future<Output = Result<Option<Identity>, Response>>;
    fn end_session(
        &self,
        subject: String,
        session: String,
        now: u64,
    ) -> impl Future<Output = Result<(), Response>>;
}
pub fn digest(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}
pub fn same_secret(a: &str, b: &str) -> bool {
    bool::from(Sha256::digest(a.as_bytes()).ct_eq(&Sha256::digest(b.as_bytes())))
}
pub fn token_hash(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(&Sha256::digest(value.as_bytes())[..16])
}
pub fn unavailable() -> Response {
    Response::error(
        503,
        "temporarily_unavailable",
        "Identity service unavailable",
    )
}
pub fn invalid_grant() -> Response {
    Response::error(400, "invalid_grant", "Grant expired, consumed or invalid")
}
