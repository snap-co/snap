//! WebAuthn ceremonies. Identity stores private verifier state, binds continuations
//! to a caller secret, and commits verified credentials and sessions together.
use crate::{Credential, CredentialKind, Crypto, Identity, Issued, TABLES, attempt, hex};
use alloc::{format, string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snap_store::{Data, Error, Transaction};

/// Platform protocol capability. Implement with a maintained WebAuthn verifier.
/// State and material are server-only. Verification must enforce RP ID, origin,
/// challenge, user verification, signature and authenticator counter policy.
pub trait WebAuthn {
    fn register(
        &self,
        user: [u8; 16],
        name: &str,
        credentials: &[Value],
    ) -> Result<Ceremony, Error>;
    fn registered(&self, state: &Value, response: Value) -> Result<VerifiedCredential, Error>;
    fn authenticate(&self, credentials: Option<&[Value]>) -> Result<Ceremony, Error>;
    fn response_id(&self, response: &Value) -> Result<String, Error>;
    fn authenticated(&self, state: Value, response: Value, material: Value)
    -> Result<Value, Error>;
}
pub struct Ceremony {
    pub options: Value,
    pub state: Value,
}
pub struct VerifiedCredential {
    pub id: String,
    pub material: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub attempt: String,
    pub options: Value,
    pub expires: i64,
}
#[derive(Serialize, Deserialize)]
struct Attempt {
    binding: String,
    expires: i64,
    registration: bool,
    identity: Option<String>,
    #[serde(default)]
    allowed_identities: Option<Vec<String>>,
    bearer_digest: Option<Vec<u8>>,
    label: String,
    state: Value,
}
pub struct Passkeys {
    identity: Identity,
}
impl Passkeys {
    pub fn new(identity: Identity) -> Self {
        Self { identity }
    }
    pub fn data() -> Data {
        Data::new(&TABLES)
    }
    /// Anonymous enrollment creates a new identity only after verified registration.
    /// A bearer instead links to its freshly proved identity; labels never link accounts.
    pub fn begin_registration(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        webauthn: &impl WebAuthn,
        bearer: Option<&str>,
        binding: &str,
        label: &str,
        now: i64,
    ) -> Result<Challenge, Error> {
        crate::credential::label_input(label)?;
        let identity = match bearer {
            Some(b) => self.identity.fresh(tx, crypto, b, now)?.identity,
            None => hex(&crypto.random()?),
        };
        let keys = if bearer.is_some() {
            materials(tx, &identity)?
        } else {
            Vec::new()
        };
        let hash = crypto.digest(&format!("identity.webauthn-user:{identity}"));
        let user: [u8; 16] = hash
            .get(..16)
            .ok_or(Error::Unavailable)?
            .try_into()
            .map_err(|_| Error::Unavailable)?;
        let ceremony = webauthn.register(user, label, &keys)?;
        self.start(
            tx,
            crypto,
            binding,
            Attempt {
                binding: String::new(),
                expires: now.checked_add(300).ok_or(Error::Invalid)?,
                registration: true,
                identity: Some(identity),
                allowed_identities: None,
                bearer_digest: bearer.map(|b| crypto.digest(b)),
                label: label.into(),
                state: ceremony.state,
            },
            ceremony.options,
            now,
        )
    }
    /// None selects discoverable credentials. Named authentication accepts a
    /// credential locator, not a session ID or an identity selected for authority.
    pub fn begin_authentication(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        webauthn: &impl WebAuthn,
        locator: Option<&str>,
        binding: &str,
        now: i64,
    ) -> Result<Challenge, Error> {
        let identity = locator
            .map(|l| {
                Credential::lookup(tx, l)?
                    .map(|c| String::from(c.identity()))
                    .ok_or(Error::NotFound)
            })
            .transpose()?;
        let identities = identity.map(|i| alloc::vec![i]);
        self.begin_authentication_for(tx, crypto, webauthn, identities.as_deref(), binding, now)
    }
    /// Trusted host name lookup supplies candidate principals, not authority.
    /// Several accounts may share metadata; only a verified assertion chooses its
    /// credential owner. None requests discoverable authentication.
    pub fn begin_authentication_for(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        webauthn: &impl WebAuthn,
        identities: Option<&[String]>,
        binding: &str,
        now: i64,
    ) -> Result<Challenge, Error> {
        let keys = identities
            .map(|identities| {
                let mut keys = Vec::new();
                for identity in identities {
                    keys.extend(materials(tx, identity)?);
                }
                Ok::<_, Error>(keys)
            })
            .transpose()?;
        if keys.as_ref().is_some_and(Vec::is_empty) {
            return Err(Error::NotFound);
        }
        let ceremony = webauthn.authenticate(keys.as_deref())?;
        self.start(
            tx,
            crypto,
            binding,
            Attempt {
                binding: String::new(),
                expires: now.checked_add(300).ok_or(Error::Invalid)?,
                registration: false,
                identity: None,
                allowed_identities: identities.map(<[String]>::to_vec),
                bearer_digest: None,
                label: String::new(),
                state: ceremony.state,
            },
            ceremony.options,
            now,
        )
    }
    fn start(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        binding: &str,
        mut pending: Attempt,
        options: Value,
        now: i64,
    ) -> Result<Challenge, Error> {
        if now < 0 || binding.len() < 32 || binding.len() > 256 {
            return Err(Error::Invalid);
        }
        pending.binding = hex(&crypto.digest(binding));
        let mut active = 0;
        for row in tx.find(attempt::TABLE, "primary", &[])? {
            let id = crate::text(&row, "id")?;
            if !id.starts_with("passkey:") {
                continue;
            }
            let old: Attempt = attempt::read(tx, attempt::TABLE, id)?.ok_or(Error::Invalid)?;
            if old.expires <= now {
                tx.delete(attempt::TABLE, &[id.into()])?;
            } else {
                active += 1;
            }
        }
        if active >= 128 {
            return Err(Error::Unavailable);
        }
        let id = format!("passkey:{}", hex(&crypto.random()?));
        attempt::write(tx, attempt::TABLE, &id, &pending, true)?;
        Ok(Challenge {
            attempt: id,
            options,
            expires: pending.expires,
        })
    }
    /// Successful completion consumes the attempt and updates material atomically.
    /// Invalid proofs do not issue authority. A rolled-back completion may be retried.
    pub fn finish_registration(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        webauthn: &impl WebAuthn,
        id: &str,
        binding: &str,
        bearer: Option<&str>,
        response: Value,
        now: i64,
    ) -> Result<Issued, Error> {
        let pending = self.pending(tx, crypto, id, binding, true, now)?;
        let identity = pending.identity.ok_or(Error::Invalid)?;
        match (&pending.bearer_digest, bearer) {
            (Some(digest), Some(b)) if digest == &crypto.digest(b) => {
                if self.identity.fresh(tx, crypto, b, now)?.identity != identity {
                    return Err(Error::NotFound);
                }
            }
            (None, None) => {
                tx.insert(TABLES[0], crate::row([("id", identity.clone().into())]))?;
            }
            _ => return Err(Error::NotFound),
        }
        let verified = webauthn.registered(&pending.state, response)?;
        let locator = key_locator(&verified.id)?;
        Credential::insert(
            tx,
            &locator,
            &identity,
            CredentialKind::Passkey,
            &serde_json::to_string(&verified.material).map_err(|_| Error::Invalid)?,
            &pending.label,
        )?;
        tx.delete(attempt::TABLE, &[id.into()])?;
        crate::session::Sessions::issue(tx, crypto, &identity, now, self.identity.lifetime_seconds)
    }
    pub fn finish_authentication(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        webauthn: &impl WebAuthn,
        id: &str,
        binding: &str,
        response: Value,
        now: i64,
    ) -> Result<Issued, Error> {
        let pending = self.pending(tx, crypto, id, binding, false, now)?;
        let locator = key_locator(&webauthn.response_id(&response)?)?;
        let credential = Credential::lookup(tx, &locator)?.ok_or(Error::NotFound)?;
        if credential.kind() != CredentialKind::Passkey
            || pending
                .allowed_identities
                .as_ref()
                .is_some_and(|identities| !identities.iter().any(|i| i == credential.identity()))
            || pending
                .identity
                .as_ref()
                .is_some_and(|i| i != credential.identity())
        {
            return Err(Error::NotFound);
        }
        let material = serde_json::from_str(credential.material()).map_err(|_| Error::Invalid)?;
        let updated = webauthn.authenticated(pending.state, response, material)?;
        Credential::update_material(
            tx,
            &locator,
            &serde_json::to_string(&updated).map_err(|_| Error::Invalid)?,
        )?;
        tx.delete(attempt::TABLE, &[id.into()])?;
        crate::session::Sessions::issue(
            tx,
            crypto,
            credential.identity(),
            now,
            self.identity.lifetime_seconds,
        )
    }
    fn pending(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        id: &str,
        binding: &str,
        registration: bool,
        now: i64,
    ) -> Result<Attempt, Error> {
        if !id.starts_with("passkey:") || now < 0 {
            return Err(Error::Invalid);
        }
        let pending: Attempt = attempt::read(tx, attempt::TABLE, id)?.ok_or(Error::NotFound)?;
        if pending.expires <= now
            || pending.registration != registration
            || !crate::oauth::same_secret(&pending.binding, &hex(&crypto.digest(binding)))
        {
            return Err(Error::NotFound);
        }
        Ok(pending)
    }
}
fn key_locator(id: &str) -> Result<String, Error> {
    if id.is_empty()
        || id.len() > 2048
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(Error::Invalid);
    }
    Ok(format!("passkey:{id}"))
}
fn materials(tx: &mut Transaction<'_>, identity: &str) -> Result<Vec<Value>, Error> {
    Credential::passkeys(tx, identity)?
        .iter()
        .map(|c| serde_json::from_str(c.material()).map_err(|_| Error::Invalid))
        .collect()
}
