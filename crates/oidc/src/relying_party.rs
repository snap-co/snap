//! Relying-party transaction boundaries. Hosts own HTTP and signature verification.
//! Persist consumption before code/refresh IO; an uncertain exchange is never retried.
//! Session and attempt records are private server data, never Document payloads.
use alloc::{
    format,
    string::{String, ToString},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use snap_store::{Error, Transaction};

pub const MIGRATION: &str = include_str!("../migrations/0001_oidc_rp.toml");
pub const TABLES: [&str; 2] = ["oidc_rp.attempts", "oidc_rp.sessions"];
const ATTEMPTS: &str = TABLES[0];
const SESSIONS: &str = TABLES[1];

pub fn digest(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}
pub fn same_secret(a: &str, b: &str) -> bool {
    let a = Sha256::digest(a.as_bytes());
    let b = Sha256::digest(b.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
pub fn owner(issuer: &str, subject: &str) -> String {
    digest(&format!("{issuer}\n{subject}"))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub binding: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect: String,
    pub issuer: String,
    pub old_session: Option<String>,
    pub logout: bool,
    pub expires: i64,
    pub processing: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// Digest of the local bearer, not an upstream token.
    pub id: String,
    pub owner: String,
    pub subject: String,
    pub issuer: String,
    pub csrf: String,
    pub nonce: String,
    pub profile: Value,
    pub tokens: Tokens,
    pub expires: i64,
    pub refreshing: bool,
    pub version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub id_token: String,
    pub access_expires: i64,
    pub auth_time: Option<i64>,
}

fn read<T: serde::de::DeserializeOwned>(
    tx: &mut Transaction<'_>,
    table: &str,
    id: &str,
) -> Result<Option<T>, Error> {
    tx.get(table, &[id.into()])?
        .map(|row| {
            let Some(snap_store::Value::Text(data)) = row.get("data") else {
                return Err(Error::Invalid);
            };
            serde_json::from_str(data).map_err(|_| Error::Invalid)
        })
        .transpose()
}
fn write<T: Serialize>(
    tx: &mut Transaction<'_>,
    table: &str,
    id: &str,
    value: &T,
    insert: bool,
) -> Result<(), Error> {
    let data = serde_json::to_string(value).map_err(|_| Error::Invalid)?;
    if insert {
        tx.insert(
            table,
            [("id".into(), id.into()), ("data".into(), data.into())]
                .into_iter()
                .collect(),
        )
    } else {
        tx.update(
            table,
            &[id.into()],
            [("data".into(), data.into())].into_iter().collect(),
        )
    }
}

pub fn start(tx: &mut Transaction<'_>, state: &str, attempt: &Attempt) -> Result<(), Error> {
    if state.len() < 32 || attempt.binding.is_empty() || attempt.expires < 0 || attempt.processing {
        return Err(Error::Invalid);
    }
    write(tx, ATTEMPTS, &digest(state), attempt, true)
}

/// Commit the returned attempt before exchanging a code. A second call fails even
/// after host restart. A failed browser correlation leaves the rightful attempt usable.
pub fn consume(
    tx: &mut Transaction<'_>,
    state: &str,
    binding: &str,
    logout: bool,
    now: i64,
) -> Result<Attempt, Error> {
    let id = digest(state);
    let mut attempt: Attempt = read(tx, ATTEMPTS, &id)?.ok_or(Error::NotFound)?;
    if attempt.expires <= now
        || attempt.processing
        || attempt.logout != logout
        || !same_secret(&attempt.binding, &digest(binding))
    {
        return Err(Error::NotFound);
    }
    attempt.processing = true;
    write(tx, ATTEMPTS, &id, &attempt, false)?;
    Ok(attempt)
}

pub fn issue(
    tx: &mut Transaction<'_>,
    state: &str,
    session: &Session,
    now: i64,
) -> Result<(), Error> {
    let attempt: Attempt = read(tx, ATTEMPTS, &digest(state))?.ok_or(Error::NotFound)?;
    if !attempt.processing
        || attempt.logout
        || attempt.expires <= now
        || attempt.issuer != session.issuer
        || attempt.nonce != session.nonce
        || session.owner != owner(&session.issuer, &session.subject)
        || session.refreshing
        || session.expires <= now
    {
        return Err(Error::Invalid);
    }
    if let Some(old) = attempt.old_session {
        tx.delete(SESSIONS, &[old.into()])?;
    }
    write(tx, SESSIONS, &session.id, session, true)?;
    tx.delete(ATTEMPTS, &[digest(state).into()])?;
    Ok(())
}

/// A refreshing session has no usable authority. Hosts arrange an explicit refresh
/// before accepting protected commands; a failed exchange requires a fresh login.
pub fn resolve(tx: &mut Transaction<'_>, bearer: &str, now: i64) -> Result<Session, Error> {
    resolve_id(tx, &digest(bearer), now)
}
pub fn resolve_id(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<Session, Error> {
    let session: Session = read(tx, SESSIONS, id)?.ok_or(Error::NotFound)?;
    if session.expires <= now || session.tokens.access_expires <= now || session.refreshing {
        return Err(Error::NotFound);
    }
    Ok(session)
}

/// Authority for already accepted work and connected Document operations may
/// survive an in-flight refresh while the old access token remains valid. Local
/// logout, access expiry or failed refresh immediately fences further publication.
pub fn lease(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<Session, Error> {
    let session: Session = read(tx, SESSIONS, id)?.ok_or(Error::NotFound)?;
    if session.expires <= now || session.tokens.access_expires <= now {
        return Err(Error::NotFound);
    }
    Ok(session)
}

/// Run once before opening listeners. Lost exchanges are not replayed and cannot
/// retain authority through an old access token after restart.
pub fn recover(tx: &mut Transaction<'_>, now: i64) -> Result<(), Error> {
    for row in tx.find(SESSIONS, "primary", &[])? {
        let Some(snap_store::Value::Text(id)) = row.get("id") else {
            return Err(Error::Invalid);
        };
        let session: Session = read(tx, SESSIONS, id)?.ok_or(Error::Invalid)?;
        if session.refreshing || session.expires <= now {
            tx.delete(SESSIONS, &[id.clone().into()])?;
        }
    }
    for row in tx.find(ATTEMPTS, "primary", &[])? {
        let Some(snap_store::Value::Text(id)) = row.get("id") else {
            return Err(Error::Invalid);
        };
        let attempt: Attempt = read(tx, ATTEMPTS, id)?.ok_or(Error::Invalid)?;
        if attempt.processing || attempt.expires <= now {
            tx.delete(ATTEMPTS, &[id.clone().into()])?;
        }
    }
    Ok(())
}

/// Returns None while tokens remain fresh. Otherwise fences the session and returns
/// the single owned refresh request. Never automatically release this fence on restart.
pub fn begin_refresh(
    tx: &mut Transaction<'_>,
    bearer: &str,
    now: i64,
) -> Result<Option<Session>, Error> {
    begin_refresh_id(tx, &digest(bearer), now)
}

/// Native hosts can refresh a session selected by an application credential.
/// The host must validate that credential before resolving this private ID.
pub fn begin_refresh_id(
    tx: &mut Transaction<'_>,
    id: &str,
    now: i64,
) -> Result<Option<Session>, Error> {
    let mut session: Session = read(tx, SESSIONS, id)?.ok_or(Error::NotFound)?;
    if session.expires <= now || session.refreshing {
        return Err(Error::NotFound);
    }
    if session.tokens.access_expires > now.saturating_add(30) {
        return Ok(None);
    }
    session.refreshing = true;
    write(tx, SESSIONS, id, &session, false)?;
    Ok(Some(session))
}

pub fn finish_refresh(
    tx: &mut Transaction<'_>,
    previous: &Session,
    tokens: Tokens,
    now: i64,
) -> Result<Session, Error> {
    let mut session: Session = read(tx, SESSIONS, &previous.id)?.ok_or(Error::NotFound)?;
    if !session.refreshing
        || session.version != previous.version
        || session.expires <= now
        || tokens.access_expires <= now
    {
        return Err(Error::NotFound);
    }
    session.tokens = tokens;
    session.version = session.version.checked_add(1).ok_or(Error::Invalid)?;
    session.refreshing = false;
    write(tx, SESSIONS, &session.id, &session, false)?;
    Ok(session)
}

pub fn revoke(tx: &mut Transaction<'_>, bearer: &str) -> Result<(), Error> {
    revoke_id(tx, &digest(bearer))
}

pub fn revoke_id(tx: &mut Transaction<'_>, id: &str) -> Result<(), Error> {
    tx.delete(SESSIONS, &[id.into()])?;
    Ok(())
}
/// Local logout may revoke an expired upstream grant or an uncertain refresh.
/// This method is not authority for application reads or writes.
pub fn for_logout(tx: &mut Transaction<'_>, bearer: &str, now: i64) -> Result<Session, Error> {
    let session: Session = read(tx, SESSIONS, &digest(bearer))?.ok_or(Error::NotFound)?;
    if session.expires <= now {
        return Err(Error::NotFound);
    }
    Ok(session)
}

/// Cancel this browser's previous continuation and discard expired attempts.
pub fn clear_attempts(
    tx: &mut Transaction<'_>,
    binding: Option<&str>,
    now: i64,
) -> Result<(), Error> {
    for row in tx.find(ATTEMPTS, "primary", &[])? {
        let Some(snap_store::Value::Text(id)) = row.get("id") else {
            return Err(Error::Invalid);
        };
        let attempt: Attempt = read(tx, ATTEMPTS, id)?.ok_or(Error::Invalid)?;
        if attempt.expires <= now
            || binding.is_some_and(|b| same_secret(&attempt.binding, &digest(b)))
        {
            tx.delete(ATTEMPTS, &[id.clone().into()])?;
        }
    }
    Ok(())
}
pub fn finish_logout(tx: &mut Transaction<'_>, state: &str) -> Result<(), Error> {
    tx.delete(ATTEMPTS, &[digest(state).into()])?;
    Ok(())
}

/// Validate already signature-verified claims. The native adapter must pin RS256,
/// fetch JWKS only from the configured issuer, and reject redirects on token IO.
pub struct Validation<'a> {
    pub issuer: &'a str,
    pub client: &'a str,
    pub nonce: Option<&'a str>,
    pub previous: Option<&'a Session>,
    pub now: i64,
}
pub fn validate_tokens(
    response: &Value,
    claims: &Value,
    context: Validation<'_>,
) -> Result<Tokens, Error> {
    let Validation {
        issuer,
        client,
        nonce,
        previous,
        now,
    } = context;
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 255 && s.is_ascii())
        .ok_or(Error::Invalid)?;
    let audience = claims["aud"].as_str() == Some(client)
        || claims["aud"]
            .as_array()
            .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some(client));
    if claims["iss"].as_str() != Some(issuer)
        || !audience
        || claims["exp"].as_i64().is_none_or(|t| t <= now)
        || claims["iat"]
            .as_i64()
            .is_none_or(|t| t < 0 || t > now.saturating_add(30) || now.saturating_sub(t) > 900)
        || claims
            .get("azp")
            .is_some_and(|a| a.as_str() != Some(client))
        || nonce.is_some_and(|nonce| {
            claims["nonce"]
                .as_str()
                .is_none_or(|n| !same_secret(n, nonce))
        })
        || previous.is_some_and(|old| {
            old.subject != subject
                || claims
                    .get("nonce")
                    .is_some_and(|n| n.as_str() != Some(&old.nonce))
                || claims
                    .get("auth_time")
                    .is_some_and(|t| t.as_i64() != old.tokens.auth_time)
        })
    {
        return Err(Error::Invalid);
    }
    let access = response["access_token"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() < 8192 && s.is_ascii())
        .ok_or(Error::Invalid)?;
    let refresh = response["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() < 8192 && s.is_ascii())
        .ok_or(Error::Invalid)?;
    let id_token = response["id_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(Error::Invalid)?;
    if response["token_type"]
        .as_str()
        .is_none_or(|s| !s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(Error::Invalid);
    }
    let hash = URL_SAFE_NO_PAD.encode(&Sha256::digest(access.as_bytes())[..16]);
    if claims
        .get("at_hash")
        .is_some_and(|h| h.as_str().is_none_or(|h| !same_secret(h, &hash)))
    {
        return Err(Error::Invalid);
    }
    let ttl = response["expires_in"]
        .as_i64()
        .filter(|t| *t > 0 && *t <= 86400)
        .ok_or(Error::Invalid)?;
    Ok(Tokens {
        access: access.into(),
        refresh: refresh.into(),
        id_token: id_token.to_string(),
        access_expires: now.checked_add(ttl).ok_or(Error::Invalid)?,
        auth_time: claims["auth_time"].as_i64(),
    })
}
