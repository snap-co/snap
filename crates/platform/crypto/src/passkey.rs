//! Native WebAuthn proof verification. Ceremony and credential policy stay in
//! Identity; this capability validates signatures, origins and authenticator data.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use snap_identity::passkey::{Ceremony, VerifiedCredential, WebAuthn};
use snap_store::Error;
use webauthn_rs::prelude::*;

#[derive(Clone)]
pub struct Native(Webauthn);
impl Native {
    /// RP ID and exact allowed origin are app-host configuration. Do not enable
    /// subdomain or arbitrary-port matching based on a request header.
    pub fn new(rp_id: &str, origin: &str) -> Result<Self, Error> {
        let url = Url::parse(origin).map_err(|_| Error::Invalid)?;
        let local = url.domain() == Some("localhost");
        if url.origin().ascii_serialization() != origin
            || !(url.scheme() == "https" || (url.scheme() == "http" && local))
        {
            return Err(Error::Invalid);
        }
        WebauthnBuilder::new(rp_id, &url)
            .map_err(|_| Error::Invalid)?
            .build()
            .map(Self)
            .map_err(|_| Error::Invalid)
    }
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::Invalid)
}
fn encode<T: serde::Serialize>(value: T) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::Unavailable)
}
#[derive(serde::Serialize, serde::Deserialize)]
enum Authentication {
    Named(PasskeyAuthentication),
    Discoverable(DiscoverableAuthentication),
}
impl WebAuthn for Native {
    fn register(
        &self,
        user: [u8; 16],
        name: &str,
        credentials: &[Value],
    ) -> Result<Ceremony, Error> {
        let keys: Vec<Passkey> = credentials
            .iter()
            .cloned()
            .map(decode)
            .collect::<Result<_, _>>()?;
        let excludes = keys.iter().map(|k| k.cred_id().clone()).collect();
        let (options, state) = self
            .0
            .start_passkey_registration(Uuid::from_bytes(user), name, name, Some(excludes))
            .map_err(|_| Error::Invalid)?;
        Ok(Ceremony {
            options: encode(options)?,
            state: encode(state)?,
        })
    }
    fn registered(&self, state: &Value, response: Value) -> Result<VerifiedCredential, Error> {
        let key = self
            .0
            .finish_passkey_registration(&decode(response)?, &decode(state.clone())?)
            .map_err(|_| Error::NotFound)?;
        Ok(VerifiedCredential {
            id: URL_SAFE_NO_PAD.encode(key.cred_id().as_ref()),
            material: encode(key)?,
        })
    }
    fn authenticate(&self, credentials: Option<&[Value]>) -> Result<Ceremony, Error> {
        match credentials {
            Some(credentials) => {
                let keys: Vec<Passkey> = credentials
                    .iter()
                    .cloned()
                    .map(decode)
                    .collect::<Result<_, _>>()?;
                let (options, state) = self
                    .0
                    .start_passkey_authentication(&keys)
                    .map_err(|_| Error::NotFound)?;
                Ok(Ceremony {
                    options: encode(options)?,
                    state: encode(Authentication::Named(state))?,
                })
            }
            None => {
                let (options, state) = self
                    .0
                    .start_discoverable_authentication()
                    .map_err(|_| Error::Unavailable)?;
                Ok(Ceremony {
                    options: encode(options)?,
                    state: encode(Authentication::Discoverable(state))?,
                })
            }
        }
    }
    fn response_id(&self, response: &Value) -> Result<String, Error> {
        let response: PublicKeyCredential = decode(response.clone())?;
        Ok(URL_SAFE_NO_PAD.encode(response.raw_id.as_ref()))
    }
    fn authenticated(
        &self,
        state: Value,
        response: Value,
        material: Value,
    ) -> Result<Value, Error> {
        let response: PublicKeyCredential = decode(response)?;
        let mut key: Passkey = decode(material)?;
        let result = match decode::<Authentication>(state)? {
            Authentication::Named(state) => self.0.finish_passkey_authentication(&response, &state),
            Authentication::Discoverable(state) => self.0.finish_discoverable_authentication(
                &response,
                state,
                &[DiscoverableKey::from(&key)],
            ),
        }
        .map_err(|_| Error::NotFound)?;
        if result.cred_id() != key.cred_id() || !result.user_verified() {
            return Err(Error::NotFound);
        }
        let current: Credential = key.clone().into();
        if (current.counter > 0 || result.counter() > 0) && result.counter() <= current.counter {
            return Err(Error::NotFound);
        }
        key.update_credential(&result).ok_or(Error::Invalid)?;
        encode(key)
    }
}
