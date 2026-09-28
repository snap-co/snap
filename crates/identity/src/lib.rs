//! Credentials and sessions in the caller's Store transaction. Hosts supply crypto
//! and Unix time in seconds. No bearer is usable until the enclosing commit succeeds.
#![no_std]
extern crate alloc;
pub mod operation;

use alloc::{format, string::String, vec::Vec};
use snap_store::{Error, Row, Transaction, Value};

pub const MIGRATION: &str = include_str!("../migrations/0001_identity.toml");
pub const TABLES: [&str; 3] = [
    "identity.identities",
    "identity.credentials",
    "identity.sessions",
];

/// Hosts must use cryptographically secure randomness and password hashing.
/// Test adapters may be deterministic. Implementations must not log secrets.
pub trait Crypto {
    fn random(&mut self) -> Result<[u8; 32], Error>;
    fn hash_password(&mut self, password: &str) -> Result<String, Error>;
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error>;
    fn digest(&self, secret: &str) -> Vec<u8>;
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Session {
    pub identity: String,
    pub expires: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SessionSummary {
    /// Domain-separated digest identifier, never a bearer or stored session key.
    pub id: String,
    pub expires: i64,
    pub current: bool,
}

/// Deliberately has no Debug/Serialize implementation. Release the bearer only
/// from Store's Committed result; neither records nor diagnostics may retain it.
pub struct Issued {
    pub bearer: String,
    pub session: Session,
}

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

    /// ASCII email addresses are trimmed and lowercased. Passwords are 8..=1024
    /// UTF-8 bytes. This is an explicit initial policy, not email verification.
    pub fn enroll(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        email: &str,
        password: &str,
        now: i64,
    ) -> Result<Issued, Error> {
        let email = email_key(email)?;
        password_input(password)?;
        if tx.get(TABLES[1], &[email.clone().into()])?.is_some() {
            return Err(Error::Constraint);
        }
        let id = hex(&crypto.random()?);
        let hash = crypto.hash_password(password)?;
        tx.insert(TABLES[0], row([("id", id.clone().into())]))?;
        tx.insert(
            TABLES[1],
            row([
                ("email", email.into()),
                ("identity", id.clone().into()),
                ("hash", hash.into()),
            ]),
        )?;
        self.issue(tx, crypto, id, now)
    }

    /// Unknown email and wrong password both return NotFound. A residency miss
    /// remains a miss. Verification runs inside this transaction's exclusion, so
    /// the verified credential cannot change between verification and issuance.
    pub fn login(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        email: &str,
        password: &str,
        now: i64,
    ) -> Result<Issued, Error> {
        let email = email_key(email)?;
        password_input(password)?;
        let credential = tx.get(TABLES[1], &[email.into()])?.ok_or(Error::NotFound)?;
        if !crypto.verify_password(password, text(&credential, "hash")?)? {
            return Err(Error::NotFound);
        }
        self.issue(tx, crypto, text(&credential, "identity")?.into(), now)
    }

    fn issue(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        identity: String,
        now: i64,
    ) -> Result<Issued, Error> {
        if now < 0 {
            return Err(Error::Invalid);
        }
        let expires = now
            .checked_add(self.lifetime_seconds)
            .ok_or(Error::Invalid)?;
        let bearer = hex(&crypto.random()?);
        tx.insert(
            TABLES[2],
            row([
                ("digest", Value::Bytes(crypto.digest(&bearer))),
                ("identity", identity.clone().into()),
                ("expires", expires.into()),
            ]),
        )?;
        Ok(Issued {
            bearer,
            session: Session { identity, expires },
        })
    }

    pub fn resolve(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Session, Error> {
        if now < 0 {
            return Err(Error::Invalid);
        }
        if bearer.len() != 64 || !bearer.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::NotFound);
        }
        self.resolve_digest(tx, &crypto.digest(bearer), now)
    }

    /// Internal durable session reference for modules such as OAuth grant
    /// families. A digest is not a wire credential: hosts must never accept a
    /// caller-supplied digest in place of a bearer or expose it in client views.
    pub fn resolve_digest(
        &self,
        tx: &mut Transaction<'_>,
        digest: &[u8],
        now: i64,
    ) -> Result<Session, Error> {
        if now < 0 || digest.is_empty() {
            return Err(Error::Invalid);
        }
        let session = tx
            .get(TABLES[2], &[Value::Bytes(digest.into())])?
            .ok_or(Error::NotFound)?;
        let Some(Value::Integer(expires)) = session.get("expires") else {
            return Err(Error::Invalid);
        };
        if now >= *expires {
            return Err(Error::NotFound);
        }
        Ok(Session {
            identity: text(&session, "identity")?.into(),
            expires: *expires,
        })
    }

    /// Revokes only this session. Use the same transaction to check authority for
    /// application writes. Disconnect is a transport action, not session revocation.
    pub fn revoke(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<(), Error> {
        self.resolve(tx, crypto, bearer, now)?;
        tx.delete(TABLES[2], &[Value::Bytes(crypto.digest(bearer))])
            .map(|_| ())
    }

    pub fn sessions(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Vec<SessionSummary>, Error> {
        let actor = self.resolve(tx, crypto, bearer, now)?;
        let current = crypto.digest(bearer);
        self.sessions_for(tx, crypto, &actor.identity, &current, now)
    }

    /// Trusted composition with authority captured before ACK. `current` is the
    /// private bearer digest, never a caller-selected session identifier.
    pub fn sessions_for(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        identity: &str,
        current: &[u8],
        now: i64,
    ) -> Result<Vec<SessionSummary>, Error> {
        let mut sessions = Vec::new();
        for row in tx.find(TABLES[2], "primary", &[])? {
            if text(&row, "identity")? != identity {
                continue;
            }
            let (Some(Value::Bytes(digest)), Some(Value::Integer(expires))) =
                (row.get("digest"), row.get("expires"))
            else {
                return Err(Error::Invalid);
            };
            if *expires <= now {
                continue;
            }
            sessions.push(SessionSummary {
                id: session_id(crypto, digest),
                expires: *expires,
                current: *digest == current,
            });
        }
        Ok(sessions)
    }

    /// Credential labels only. Password hashes and bearer material never leave
    /// this transaction boundary as part of account observations.
    pub fn credentials(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Vec<String>, Error> {
        let actor = self.resolve(tx, crypto, bearer, now)?;
        self.credentials_for(tx, &actor.identity)
    }

    /// Credential labels for an identity authorized by the enclosing dispatcher.
    pub fn credentials_for(
        &self,
        tx: &mut Transaction<'_>,
        identity: &str,
    ) -> Result<Vec<String>, Error> {
        let mut labels = Vec::new();
        for row in tx.find(TABLES[1], "primary", &[])? {
            if text(&row, "identity")? == identity {
                labels.push(text(&row, "email")?.into());
            }
        }
        Ok(labels)
    }

    /// Revoke current, other or all sessions under the caller's current authority.
    pub fn revoke_scope(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        scope: &str,
        now: i64,
    ) -> Result<(), Error> {
        if !["current", "others", "all"].contains(&scope) {
            return Err(Error::Invalid);
        }
        let actor = self.resolve(tx, crypto, bearer, now)?;
        let current = crypto.digest(bearer);
        self.revoke_scope_for(tx, &actor.identity, &current, scope)
    }

    /// Revoke using accepted authority; expiry cannot cancel an accepted logout.
    pub fn revoke_scope_for(
        &self,
        tx: &mut Transaction<'_>,
        identity: &str,
        current: &[u8],
        scope: &str,
    ) -> Result<(), Error> {
        if !["current", "others", "all"].contains(&scope) {
            return Err(Error::Invalid);
        }
        for row in tx.find(TABLES[2], "primary", &[])? {
            if text(&row, "identity")? != identity {
                continue;
            }
            let Some(Value::Bytes(digest)) = row.get("digest") else {
                return Err(Error::Invalid);
            };
            if scope == "all"
                || (scope == "current" && *digest == current)
                || (scope == "others" && *digest != current)
            {
                tx.delete(TABLES[2], &[Value::Bytes(digest.clone())])?;
            }
        }
        Ok(())
    }

    pub fn revoke_session(
        &self,
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        id: &str,
        now: i64,
    ) -> Result<(), Error> {
        let actor = self.resolve(tx, crypto, bearer, now)?;
        for row in tx.find(TABLES[2], "primary", &[])? {
            if text(&row, "identity")? != actor.identity {
                continue;
            }
            let Some(Value::Bytes(digest)) = row.get("digest") else {
                return Err(Error::Invalid);
            };
            if session_id(crypto, digest) == id {
                tx.delete(TABLES[2], &[Value::Bytes(digest.clone())])?;
                return Ok(());
            }
        }
        Err(Error::NotFound)
    }
}

fn session_id(crypto: &impl Crypto, digest: &[u8]) -> String {
    hex(&crypto.digest(&format!("identity.session-id:{}", hex(digest))))
}

fn password_input(password: &str) -> Result<(), Error> {
    if (8..=1024).contains(&password.len()) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
fn email_key(email: &str) -> Result<String, Error> {
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
fn row<const N: usize>(fields: [(&str, Value); N]) -> Row {
    fields
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
}
fn text<'a>(row: &'a Row, key: &str) -> Result<&'a str, Error> {
    match row.get(key) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(Error::Invalid),
    }
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut result = String::new();
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}
