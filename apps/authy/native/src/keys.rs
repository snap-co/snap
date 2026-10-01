use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs8::{DecodePrivateKey, EncodePrivateKey},
    signature::{SignatureEncoding, Signer, Verifier},
    traits::PublicKeyParts,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use snap_store::{Error, Store, Value as Cell};

pub const MIGRATION: &str = include_str!("../migrations/0002_authy_host.toml");
pub const TABLE: &str = "authy.host_keys";

pub struct Keys {
    signing: RsaPrivateKey,
    cookie: Vec<u8>,
    kid: String,
    secure: bool,
}

impl Keys {
    pub fn load(
        store: &mut Store<snap_store_sqlite::Sqlite>,
        secure: bool,
        cookie_key: Option<&snap_config::Secret>,
    ) -> Result<Self, Error> {
        store.load(TABLE)?;
        let material = store
            .run("authy.keys", |tx| {
                let mut output = Vec::new();
                for purpose in ["rsa", "cookie"] {
                    let key = match tx.get(TABLE, &[purpose.into()])? {
                        Some(row) => match &row["key"] {
                            Cell::Bytes(bytes) => bytes.clone(),
                            _ => return Err(Error::Invalid),
                        },
                        None => {
                            let bytes = if purpose == "rsa" {
                                RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048)
                                    .map_err(|_| Error::Unavailable)?
                                    .to_pkcs8_der()
                                    .map_err(|_| Error::Unavailable)?
                                    .as_bytes()
                                    .to_vec()
                            } else {
                                let mut bytes = vec![0; 32];
                                rand::rngs::OsRng.fill_bytes(&mut bytes);
                                bytes
                            };
                            tx.insert(
                                TABLE,
                                [
                                    ("purpose".into(), purpose.into()),
                                    ("key".into(), Cell::Bytes(bytes.clone())),
                                ]
                                .into_iter()
                                .collect(),
                            )?;
                            bytes
                        }
                    };
                    output.push(key);
                }
                Ok(output)
            })?
            .value;
        let signing = RsaPrivateKey::from_pkcs8_der(&material[0]).map_err(|_| Error::Invalid)?;
        let cookie = cookie_key
            .map(|key| key.expose().as_bytes().to_vec())
            .unwrap_or_else(|| material[1].clone());
        if cookie.len() < 32 {
            return Err(Error::Invalid);
        }
        let kid = URL_SAFE_NO_PAD.encode(Sha256::digest(signing.n().to_bytes_be()));
        Ok(Self {
            signing,
            cookie,
            kid,
            secure,
        })
    }

    pub fn jwks(&self) -> Value {
        json!({"keys":[{"kty":"RSA","use":"sig","alg":"RS256","kid":self.kid,
            "n":URL_SAFE_NO_PAD.encode(self.signing.n().to_bytes_be()),
            "e":URL_SAFE_NO_PAD.encode(self.signing.e().to_bytes_be())}]})
    }

    pub fn sign(&self, claims: &Value) -> Result<String, Error> {
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"alg":"RS256","typ":"JWT","kid":self.kid}))
                .map_err(|_| Error::Invalid)?,
        );
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(|_| Error::Invalid)?);
        let input = format!("{header}.{payload}");
        let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(self.signing.clone());
        let signature = signer.sign(input.as_bytes());
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }

    /// Signature and exact local key only. The endpoint separately checks issuer,
    /// audience and allowed logout target. Expired ID hints are permitted by OIDC.
    pub fn verify_hint(&self, token: &str) -> Result<Value, Error> {
        let parts: Vec<_> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(Error::Invalid);
        }
        let header: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(parts[0])
                .map_err(|_| Error::Invalid)?,
        )
        .map_err(|_| Error::Invalid)?;
        if header["alg"] != "RS256" || header["kid"] != self.kid {
            return Err(Error::Invalid);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| Error::Invalid)?;
        let signature =
            rsa::pkcs1v15::Signature::try_from(signature.as_slice()).map_err(|_| Error::Invalid)?;
        rsa::pkcs1v15::VerifyingKey::<Sha256>::new(RsaPublicKey::from(&self.signing))
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .map_err(|_| Error::Invalid)?;
        serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(parts[1])
                .map_err(|_| Error::Invalid)?,
        )
        .map_err(|_| Error::Invalid)
    }

    fn name(&self) -> &'static str {
        if self.secure {
            "__Host-authy_session"
        } else {
            "authy_session"
        }
    }

    pub fn cookie(&self, bearer: Option<&str>) -> String {
        let suffix = if self.secure { "; Secure" } else { "" };
        match bearer {
            None => format!(
                "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{suffix}",
                self.name()
            ),
            Some(bearer) => {
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.cookie).expect("valid HMAC key");
                mac.update(bearer.as_bytes());
                let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
                format!(
                    "{}={bearer}.{signature}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000{suffix}",
                    self.name()
                )
            }
        }
    }

    pub fn read_cookie(&self, headers: &axum::http::HeaderMap) -> Option<String> {
        let mut found = None;
        for header in headers.get_all("cookie") {
            for part in header.to_str().ok()?.split(';') {
                let Some((name, value)) = part.trim().split_once('=') else {
                    continue;
                };
                if name != self.name() {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                let (bearer, signature) = value.split_once('.')?;
                if bearer.len() != 64 || !bearer.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.cookie).ok()?;
                mac.update(bearer.as_bytes());
                mac.verify_slice(&signature).ok()?;
                found = Some(bearer.into());
            }
        }
        found
    }
}

pub fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
