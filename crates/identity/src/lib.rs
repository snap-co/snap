//! Credential and private session interfaces. Operations compose these interfaces
//! in the caller's Store transaction; Transport publishes credentials after commit.
#![no_std]
extern crate alloc;
pub mod client;
pub mod credential;
pub mod operation;
mod session;

use alloc::{format, string::String, vec::Vec};
pub use credential::{Credential, CredentialKind, CredentialSummary};
pub use session::{ReleaseScope, SessionSummary};
use snap_store::{Data, Error, Row, Transaction, Value};
pub use snap_transport::bearer::Principal;
pub const MIGRATION: &str = include_str!("../migrations/0001_identity.toml");
/// Existing sessions retain authentication with an unknown (zero) issue time.
/// Their recorded expiry remains authoritative; fresh authentication is explicit.
pub const SESSION_TIME_MIGRATION: &str =
    include_str!("../migrations/0006_identity_session_time.toml");
pub const TABLES: [&str; 3] = [
    "identity.identities",
    "identity.credentials",
    "identity.sessions",
];

/// Hosts supply secure entropy and password hashing. Test providers may be
/// deterministic. Providers must never log passwords, hashes or bearers.
pub trait Crypto {
    fn random(&mut self) -> Result<[u8; 32], Error>;
    fn hash_password(&mut self, password: &str) -> Result<String, Error>;
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error>;
    fn digest(&self, secret: &str) -> Vec<u8>;
}
/// Trusted composition result; no Debug/Serialize. Release its bearer only after
/// the enclosing transaction commits. It contains no persisted session record.
pub struct Issued {
    pub bearer: String,
    pub principal: Principal,
}
#[derive(Clone, Copy)]
pub struct Identity {
    lifetime_seconds: i64,
}
impl Default for Identity {
    fn default() -> Self {
        Self {
            lifetime_seconds: 30 * 24 * 60 * 60,
        }
    }
}
impl Identity {
    pub fn new(lifetime_seconds: i64) -> Result<Self, Error> {
        if lifetime_seconds <= 0 {
            return Err(Error::Invalid);
        }
        Ok(Self { lifetime_seconds })
    }
    pub fn data(&self) -> Data {
        Credential::data().and(session::Sessions::data())
    }
    pub fn enroll(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        email: &str,
        password: &str,
        now: i64,
    ) -> Result<Issued, Error> {
        let credential = Credential::enroll(tx, crypto, email, password)?;
        session::Sessions::issue(
            tx,
            crypto,
            credential.identity(),
            now,
            self.lifetime_seconds,
        )
    }
    pub fn acquire(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        email: &str,
        password: &str,
        now: i64,
    ) -> Result<Issued, Error> {
        credential::password_input(password)?;
        let credential = Credential::find(tx, email)?.ok_or(Error::NotFound)?;
        credential.validate(crypto, password)?;
        session::Sessions::issue(
            tx,
            crypto,
            credential.identity(),
            now,
            self.lifetime_seconds,
        )
    }
    pub fn resolve(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Principal, Error> {
        session::Sessions::resolve(tx, crypto, bearer, now)
    }
    /// Trusted durable reference used by grant composition; never a wire credential.
    pub fn resolve_digest(
        &self,
        tx: &mut Transaction<'_>,
        digest: &[u8],
        now: i64,
    ) -> Result<Principal, Error> {
        session::Sessions::resolve_digest(tx, digest, now)
    }
    pub fn revoke(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<(), Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        self.release_accepted(
            tx,
            crypto,
            &principal.identity,
            bearer,
            ReleaseScope::Current,
        )
    }
    pub fn sessions(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Vec<SessionSummary>, Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        session::Sessions::summaries(tx, crypto, &principal.identity, &crypto.digest(bearer), now)
    }
    pub fn credentials(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Vec<CredentialSummary>, Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        Credential::summaries(tx, &principal.identity)
    }
    pub fn revoke_scope(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        scope: &str,
        now: i64,
    ) -> Result<(), Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        self.release_accepted(
            tx,
            crypto,
            &principal.identity,
            bearer,
            ReleaseScope::parse(scope)?,
        )
    }
    pub(crate) fn release_accepted(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        actor: &str,
        bearer: &str,
        scope: ReleaseScope,
    ) -> Result<(), Error> {
        session::Sessions::release(tx, actor, &crypto.digest(bearer), scope)
    }
    pub fn revoke_session(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        id: &str,
        now: i64,
    ) -> Result<(), Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        session::Sessions::release_one(tx, crypto, &principal.identity, id)
    }
    pub fn provider<C: Crypto + Send + Sync + 'static>(
        self,
        crypto: C,
    ) -> impl snap_transport::bearer::Provider {
        Provider {
            identity: self,
            crypto,
        }
    }
}
struct Provider<C> {
    identity: Identity,
    crypto: C,
}
impl<C: Crypto + Send + Sync> snap_transport::bearer::Provider for Provider<C> {
    fn data(&self) -> Data {
        session::Sessions::data()
    }
    fn identify(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
        now: i64,
    ) -> Result<Principal, Error> {
        self.identity.resolve(tx, &self.crypto, bearer, now)
    }
}

pub(crate) fn session_id(crypto: &impl Crypto, digest: &[u8]) -> String {
    hex(&crypto.digest(&format!("identity.session-id:{}", hex(digest))))
}

pub(crate) fn email_key(email: &str) -> Result<String, Error> {
    let email = email.trim();
    let mut parts = email.split('@');
    if email.len() > 254
        || !email.is_ascii()
        || email
            .bytes()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
        || parts.next().is_none_or(str::is_empty)
        || parts.next().is_none_or(str::is_empty)
        || parts.next().is_some()
    {
        return Err(Error::Invalid);
    }
    Ok(email.to_ascii_lowercase())
}
pub(crate) fn row<const N: usize>(fields: [(&str, Value); N]) -> Row {
    fields
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
}
pub(crate) fn text<'a>(row: &'a Row, key: &str) -> Result<&'a str, Error> {
    match row.get(key) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(Error::Invalid),
    }
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut result = String::new();
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}
