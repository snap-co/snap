//! Workers executes Argon2 in Wasm on its event-loop thread. Deployment CPU and
//! memory budgets must accommodate the same password policy as the native host.
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use snap_protocol::Error;
use snap_runtime::passport::{Material, unavailable};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = "
export function snapRandom() { return crypto.getRandomValues(new Uint8Array(32)); }
export function snapUuid() { return crypto.randomUUID(); }
")]
extern "C" {
    #[wasm_bindgen(catch, js_name = snapRandom)]
    fn random_bytes() -> Result<Vec<u8>, JsValue>;
    #[wasm_bindgen(catch, js_name = snapUuid)]
    fn uuid() -> Result<String, JsValue>;
}

pub fn random() -> Result<String, Error> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes().map_err(|_| unavailable())?))
}
pub fn id() -> Result<String, Error> {
    uuid().map_err(|_| unavailable())
}
pub fn now() -> u64 {
    js_sys::Date::now() as u64
}

#[derive(Clone)]
pub struct Crypto;
impl snap_runtime::passport::Crypto for Crypto {
    async fn hash(&self, password: String) -> Result<String, Error> {
        let bytes = random_bytes().map_err(|_| unavailable())?;
        let salt = SaltString::encode_b64(&bytes[..16]).map_err(|_| unavailable())?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|_| unavailable())
    }
    async fn verify(&self, password: String, hash: String) -> Result<bool, Error> {
        let hash = PasswordHash::new(&hash).map_err(|_| unavailable())?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok())
    }
    async fn generate(&self) -> Result<Material, Error> {
        let token = random()?;
        Ok(Material {
            identity: id()?,
            credential: id()?,
            session: id()?,
            digest: self.digest(&token),
            token,
        })
    }
    fn digest(&self, token: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
    }
}
