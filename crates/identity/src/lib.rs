//! Credential and private session interfaces. Operations compose these interfaces
//! in the caller's Store transaction; Transport publishes credentials after commit.
#![no_std]
extern crate alloc;
mod attempt;
pub mod authentication;
pub mod client;
pub mod credential;
pub mod oauth;
pub mod operation;
pub mod passkey;
mod session;

use alloc::{format, string::String, vec::Vec};
pub use credential::{Credential, CredentialKind, CredentialSummary};
pub use session::{ReleaseScope, SessionSummary};
use snap_store::{Data, Error, Row, Transaction, Value};
pub use snap_transport::bearer::Principal;
pub const MIGRATION: &str = include_str!("../migrations/0001_identity.toml");
pub const TABLES: [&str; 4] = [
    "identity.identities",
    "identity.credentials",
    "identity.sessions",
    "identity.attempts",
];

/// Hosts supply secure entropy and password hashing. Test providers may be
/// deterministic. Providers must never log passwords, hashes or bearers.
pub trait Crypto {
    fn random(&mut self) -> Result<[u8; 32], Error>;
    fn hash_password(&mut self, password: &str) -> Result<String, Error>;
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error>;
    /// Session key derivation. Hosts combining Identity sessions with OAuth must
    /// return the raw 32-byte SHA-256 digest of the secret's UTF-8 bytes, matching
    /// OAuth's private grant keys. Isolated test providers may use another digest.
    fn digest(&self, secret: &str) -> Vec<u8>;
    /// Verify an RS256 token's signature against a JWKS the caller already
    /// pinned to an issuer, and return its claims. Hosts own key material and
    /// signature checking; callers own claim policy. Reports `Unavailable`
    /// rather than accepting a token this host cannot check.
    fn verify_token(
        &self,
        token: &str,
        jwks: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let _ = (token, jwks);
        Err(Error::Unavailable)
    }
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
        Credential::data()
            .and(session::Sessions::data())
            .and(attempt::data())
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
    /// Credential management requires a current session and authentication within
    /// five minutes. Removing a credential never revokes existing sessions.
    pub fn remove_credential(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        locator: &str,
        now: i64,
    ) -> Result<(), Error> {
        let principal = self.fresh(tx, crypto, bearer, now)?;
        Credential::remove(tx, &principal.identity, locator)
    }
    pub fn rename_credential(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        locator: &str,
        label: &str,
        now: i64,
    ) -> Result<(), Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        Credential::rename(tx, &principal.identity, locator, label)
    }
    pub fn link_password(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        bearer: &str,
        email: &str,
        password: &str,
        now: i64,
    ) -> Result<(), Error> {
        let principal = self.fresh(tx, crypto, bearer, now)?;
        credential::password_input(password)?;
        let locator = email_key(email)?;
        Credential::insert(
            tx,
            &locator,
            &principal.identity,
            CredentialKind::Password,
            &crypto.hash_password(password)?,
            &locator,
        )
    }
    pub(crate) fn fresh(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Principal, Error> {
        let principal = self.resolve(tx, crypto, bearer, now)?;
        if principal.authenticated_at <= 0
            || principal.authenticated_at > now
            || now.saturating_sub(principal.authenticated_at) >= 300
        {
            return Err(Error::NotFound);
        }
        Ok(principal)
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
