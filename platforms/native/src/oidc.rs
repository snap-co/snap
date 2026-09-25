//! Native RS256 key lifecycle. Key generation is startup work; signing runs on a
//! blocking worker. The persisted key is never exposed through JWKS or logging.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use rsa::{
    BigUint, RsaPrivateKey, RsaPublicKey,
    pkcs1v15::{Signature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, EncodePrivateKey},
    signature::{SignatureEncoding, Signer, Verifier},
    traits::PublicKeyParts,
};
use serde_json::{Value, json};
use sha2::Sha256;
use snap_http::Response;
use snap_store::{Predicate as P, Query, Statement as S, Transaction};
use std::{io, sync::Arc};
#[derive(Clone)]
pub struct Crypto {
    key: Arc<RsaPrivateKey>,
    jwks: Value,
}
impl Crypto {
    pub fn load(store: &crate::store::Store) -> io::Result<Self> {
        use snap_oidc::storage::{SETTINGS, row};
        let q = Query::new(SETTINGS)
            .matching(vec![P::eq("key", "rs256")])
            .limit(1);
        let read = || {
            store
                .execute(Transaction {
                    guards: vec![],
                    statements: vec![S::Select(q.clone())],
                })
                .map_err(|_| io::Error::other("Cannot load OIDC signing key"))
        };
        if read()?[0].is_empty() {
            let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).map_err(io::Error::other)?;
            let encoded = B64.encode(key.to_pkcs8_der().map_err(io::Error::other)?.as_bytes());
            let result = store.execute(Transaction {
                guards: vec![],
                statements: vec![S::Insert {
                    table: SETTINGS,
                    row: row(&[("key", "rs256".into()), ("value", encoded.into())]),
                }],
            });
            if !matches!(result, Ok(_) | Err(snap_store::Error::Constraint)) {
                return Err(io::Error::other("Cannot persist OIDC signing key"));
            }
        }
        let rows = read()?;
        let Some(snap_store::Value::Text(encoded)) = rows[0].first().and_then(|r| r.get("value"))
        else {
            return Err(io::Error::other("Missing OIDC signing key"));
        };
        let key = RsaPrivateKey::from_pkcs8_der(&B64.decode(encoded).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        key.validate().map_err(io::Error::other)?;
        let n = B64.encode(key.n().to_bytes_be());
        let e = B64.encode(key.e().to_bytes_be());
        let jwks = json!({"keys":[{"kty":"RSA","use":"sig","alg":"RS256","kid":snap_oidc::digest(&n),"n":n,"e":e}]});
        Ok(Self {
            key: Arc::new(key),
            jwks,
        })
    }
}
impl snap_oidc::Crypto for Crypto {
    fn random(&self) -> Result<String, Response> {
        Ok(random())
    }
    fn jwks(&self) -> Value {
        self.jwks.clone()
    }
    async fn sign(&self, claims: Value) -> Result<String, Response> {
        let key = self.key.clone();
        let kid = self.jwks["keys"][0]["kid"].clone();
        tokio::task::spawn_blocking(move || {
            let header = B64.encode(json!({"typ":"JWT","alg":"RS256","kid":kid}).to_string());
            let payload = B64.encode(claims.to_string());
            let message = format!("{header}.{payload}");
            let signature = SigningKey::<Sha256>::new((*key).clone()).sign(message.as_bytes());
            Ok(format!("{message}.{}", B64.encode(signature.to_bytes())))
        })
        .await
        .map_err(|_| snap_oidc::unavailable())?
    }
    async fn verify(&self, token: String) -> Result<Value, Response> {
        verify(token, self.jwks.clone()).await
    }
}
pub fn random() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    B64.encode(b)
}
/// Verify only RS256 signatures against the supplied trusted issuer JWKS. Claim
/// validation belongs to the relying party, separately from signature validation.
pub async fn verify(token: String, jwks: Value) -> Result<Value, Response> {
    tokio::task::spawn_blocking(move || verify_sync(&token, &jwks))
        .await
        .map_err(|_| snap_oidc::unavailable())?
}
fn verify_sync(token: &str, jwks: &Value) -> Result<Value, Response> {
    let bad = || Response::error(400, "invalid_token", "Invalid ID token signature");
    if token.len() > 16384 {
        return Err(bad());
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(bad());
    }
    let header: Value =
        serde_json::from_slice(&B64.decode(parts[0]).map_err(|_| bad())?).map_err(|_| bad())?;
    if header["alg"] != "RS256" || header.get("crit").is_some() {
        return Err(bad());
    }
    let kid = header["kid"].as_str().ok_or_else(bad)?;
    let keys = jwks["keys"].as_array().ok_or_else(bad)?;
    let matches: Vec<_> = keys
        .iter()
        .filter(|k| k["kid"].as_str() == Some(kid))
        .collect();
    if matches.len() != 1 {
        return Err(bad());
    }
    let jwk = matches[0];
    if jwk["kty"] != "RSA" || jwk["alg"] != "RS256" || jwk["use"] != "sig" {
        return Err(bad());
    }
    let n = B64
        .decode(jwk["n"].as_str().ok_or_else(bad)?)
        .map_err(|_| bad())?;
    if !(256..=512).contains(&n.len()) {
        return Err(bad());
    }
    let e = B64
        .decode(jwk["e"].as_str().ok_or_else(bad)?)
        .map_err(|_| bad())?;
    if e.len() > 8 {
        return Err(bad());
    }
    let key = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
        .map_err(|_| bad())?;
    let signature = Signature::try_from(B64.decode(parts[2]).map_err(|_| bad())?.as_slice())
        .map_err(|_| bad())?;
    VerifyingKey::<Sha256>::new(key)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .map_err(|_| bad())?;
    serde_json::from_slice(&B64.decode(parts[1]).map_err(|_| bad())?).map_err(|_| bad())
}
