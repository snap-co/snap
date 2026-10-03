//! Native Identity cryptography. Argon2id's encoded hashes carry their parameters
//! and salt. Bearers contain 256 random bits; storage only receives SHA-256 digests.
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{
    Engine,
    engine::general_purpose::URL_SAFE_NO_PAD,
};
use rsa::{BigUint, RsaPublicKey, signature::Verifier};
use serde_json::Value;
use sha2::{Digest, Sha256};
use snap_identity::Crypto;
use snap_store::Error;

#[derive(Default)]
pub struct Native;
impl Crypto for Native {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
        Ok(bytes)
    }
    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        let salt = SaltString::encode_b64(&self.random()?[..16]).map_err(|_| Error::Unavailable)?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| Error::Unavailable)
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        let hash = PasswordHash::new(hash).map_err(|_| Error::Unavailable)?;
        match Argon2::default().verify_password(password.as_bytes(), &hash) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        Sha256::digest(secret.as_bytes()).to_vec()
    }
    fn verify_token(&self, token: &str, jwks: &Value) -> Result<Value, Error> {
        verify_rs256(token, jwks)
    }
}

/// Verify only a unique RSA signing key selected by kid. Token-controlled jku/x5u
/// are ignored; the caller fetches keys from the pinned issuer discovery document.
fn verify_rs256(token: &str, jwks: &Value) -> Result<Value, Error> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::Invalid);
    }
    let decode = |part: &str| URL_SAFE_NO_PAD.decode(part).map_err(|_| Error::Invalid);
    let header: Value = serde_json::from_slice(&decode(parts[0])?).map_err(|_| Error::Invalid)?;
    if header["alg"] != "RS256" || header.get("crit").is_some() {
        return Err(Error::Invalid);
    }
    let kid = header["kid"].as_str().ok_or(Error::Invalid)?;
    let keys: Vec<_> = jwks["keys"]
        .as_array()
        .ok_or(Error::Invalid)?
        .iter()
        .filter(|key| key["kid"].as_str() == Some(kid))
        .collect();
    if keys.len() != 1 {
        return Err(Error::Invalid);
    }
    let key = keys[0];
    if key["kty"] != "RSA"
        || key.get("alg").is_some_and(|v| v != "RS256")
        || key.get("use").is_some_and(|v| v != "sig")
        || key.get("key_ops").is_some_and(|v| {
            v.as_array()
                .is_none_or(|ops| !ops.iter().any(|op| op == "verify"))
        })
    {
        return Err(Error::Invalid);
    }
    let n = BigUint::from_bytes_be(&decode(key["n"].as_str().ok_or(Error::Invalid)?)?);
    let e = BigUint::from_bytes_be(&decode(key["e"].as_str().ok_or(Error::Invalid)?)?);
    if n.bits() < 2048 {
        return Err(Error::Invalid);
    }
    let public = RsaPublicKey::new(n, e).map_err(|_| Error::Invalid)?;
    let signature = decode(parts[2])?;
    let signature =
        rsa::pkcs1v15::Signature::try_from(signature.as_slice()).map_err(|_| Error::Invalid)?;
    rsa::pkcs1v15::VerifyingKey::<Sha256>::new(public)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .map_err(|_| Error::Invalid)?;
    serde_json::from_slice(&decode(parts[1])?).map_err(|_| Error::Invalid)
}
