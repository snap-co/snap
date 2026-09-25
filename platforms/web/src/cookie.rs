//! Signed cookie projection for the selected web carrier, not an Identity contract.
use alloc::{format, string::String, vec::Vec};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use snap_protocol::Error;

#[derive(Clone)]
pub struct Cookie {
    key: Vec<u8>,
    name: String,
    secure: bool,
    max_age: u64,
}
impl Cookie {
    pub fn new(key: Vec<u8>, name: &str, secure: bool, max_age: u64) -> Result<Self, Error> {
        if key.len() < 32
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::ContractViolationError {
                message: "Invalid cookie configuration".into(),
            });
        }
        Ok(Self {
            key,
            name: format!("{}{name}_session", if secure { "__Host-" } else { "" }),
            secure,
            max_age,
        })
    }
    pub fn read<'a>(
        &self,
        headers: impl IntoIterator<Item = &'a str>,
    ) -> Result<Option<String>, Error> {
        let mut found = None;
        for header in headers {
            for part in header.split(';') {
                let Some((name, value)) = part.trim().split_once('=') else {
                    continue;
                };
                if name != self.name {
                    continue;
                }
                if found.is_some() {
                    return Err(Error::InvalidInputError {
                        message: "invalid session cookie".into(),
                    });
                }
                found = Some(value);
            }
        }
        let Some((token, signature)) = found.and_then(|v| v.rsplit_once('.')) else {
            return Ok(None);
        };
        let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
            return Ok(None);
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC key");
        mac.update(token.as_bytes());
        Ok(mac.verify_slice(&signature).is_ok().then(|| token.into()))
    }
    pub fn encode(&self, token: Option<&str>) -> String {
        let value = token
            .map(|token| {
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC key");
                mac.update(token.as_bytes());
                format!(
                    "{token}.{}",
                    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
                )
            })
            .unwrap_or_default();
        format!(
            "{}={value}; Path=/; HttpOnly; SameSite=Lax{}; Max-Age={}",
            self.name,
            if self.secure { "; Secure" } else { "" },
            if token.is_some() { self.max_age } else { 0 }
        )
    }
}
