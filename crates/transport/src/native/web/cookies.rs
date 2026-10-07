//! HTTP-only credential encoding. Store supplies the signing key at bootstrap;
//! neither the codec nor its key table defines authentication or session policy.
use super::ReadCookie;
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use snap_store::{Backend, Error, Store};
use std::sync::Arc;

#[derive(Clone)]
pub struct Cookies {
    key: Vec<u8>,
    name: String,
    secure: bool,
}
impl Cookies {
    pub const MIGRATION: &'static str = include_str!("../../../migrations/0001_oauth_host.toml");
    pub const KEY_TABLE: &'static str = "oauth_host.keys";

    /// Reuses the existing persisted key and wire format across host restarts.
    /// Any Store backend may supply it; bootstrap must finish before listening.
    pub fn load<B: Backend>(
        store: &mut Store<B>,
        application: &str,
        secure: bool,
    ) -> Result<Self, Error> {
        if application.is_empty()
            || !application
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            return Err(Error::Invalid);
        }
        store.load(Self::KEY_TABLE)?;
        let key = store
            .run("http.cookie-keys", |tx| {
                if let Some(row) = tx.get(Self::KEY_TABLE, &["cookie".into()])? {
                    return match &row["key"] {
                        snap_store::Value::Bytes(bytes) if bytes.len() >= 32 => Ok(bytes.clone()),
                        _ => Err(Error::Invalid),
                    };
                }
                let mut bytes = vec![0; 32];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                tx.insert(
                    Self::KEY_TABLE,
                    [
                        ("id".into(), "cookie".into()),
                        ("key".into(), snap_store::Value::Bytes(bytes.clone())),
                    ]
                    .into_iter()
                    .collect(),
                )?;
                Ok(bytes)
            })?
            .value;
        Ok(Self {
            key,
            name: application.into(),
            secure,
        })
    }
    fn name(&self, correlation: bool) -> String {
        format!(
            "{}{}_{}",
            if self.secure { "__Host-" } else { "" },
            self.name,
            if correlation { "login" } else { "session" }
        )
    }
    pub fn encode(&self, bearer: Option<&str>, correlation: bool) -> String {
        let name = self.name(correlation);
        let value = bearer
            .map(|b| {
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC key");
                mac.update(name.as_bytes());
                mac.update(b.as_bytes());
                format!(
                    "{b}.{}",
                    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
                )
            })
            .unwrap_or_default();
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
            if bearer.is_none() {
                0
            } else if correlation {
                300
            } else {
                2592000
            },
            if self.secure { "; Secure" } else { "" }
        )
    }
    pub fn read(&self, headers: &HeaderMap, correlation: bool) -> Option<String> {
        let name = self.name(correlation);
        let mut found = None;
        for header in headers.get_all("cookie") {
            for part in header.to_str().ok()?.split(';') {
                let Some((key, value)) = part.trim().split_once('=') else {
                    continue;
                };
                if key != name {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                let (bearer, signature) = value.split_once('.')?;
                if !matches!(bearer.len(), 43 | 64)
                    || !bearer
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                {
                    return None;
                }
                let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
                mac.update(name.as_bytes());
                mac.update(bearer.as_bytes());
                mac.verify_slice(&signature).ok()?;
                found = Some(bearer.into());
            }
        }
        found
    }
    pub fn reader(&self) -> ReadCookie {
        let cookies = self.clone();
        Arc::new(move |headers| cookies.read(headers, false))
    }
}
