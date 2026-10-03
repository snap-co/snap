//! OAuth credentials and upstream grants. Hosts own HTTP and signature verification.
//! Persist consumption before code/refresh IO; an uncertain exchange is never retried.
//! Session and attempt records are private server data, never Document payloads.
use crate::attempt::{read, write};
use alloc::{
    format,
    string::{String, ToString},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use snap_store::{Error, Transaction};

pub const MIGRATION: &str = include_str!("../migrations/oauth/0001_oidc_rp.toml");
/// Append Identity to an existing OAuth-only host history. Use this OR Identity's
/// password migration chain, never both. Old grants lack local session proof and
/// require a new login; issuer/subject principal IDs and application data survive.
pub const IDENTITY_MIGRATION: &str = include_str!("../migrations/oauth/0008_identity_oauth.toml");
// Historical table names remain stable on disk. The grant table is not session
// authority: every read also resolves Identity's private session.
pub const TABLES: [&str; 6] = [
    "identity.attempts",
    "oidc_rp.sessions",
    "identity.identities",
    "identity.credentials",
    "identity.sessions",
    "identity.legacy_owners",
];
/// Store residency of the relying-party data interface.
pub fn data() -> snap_store::Data {
    snap_store::Data::new(&TABLES)
}
const ATTEMPTS: &str = TABLES[0];
const SESSIONS: &str = TABLES[1];
const LEGACY: &str = TABLES[5];
const IMPORTED: &str = "imported";

/// One-time ownership import, before listeners or recovery. Hosts supply every
/// principal retained by their application/ACL data, including logged-out owners.
/// Existing grants supply their historical principals too. This reserves names,
/// never issues sessions, and is not repeated after credential deletion.
pub fn import_legacy_owners(tx: &mut Transaction<'_>, owners: &[String]) -> Result<(), Error> {
    if tx.get(LEGACY, &[IMPORTED.into()])?.is_some() {
        return Ok(());
    }
    let mut owners = owners.to_vec();
    for row in tx.find(SESSIONS, "primary", &[])? {
        let grant: Grant =
            serde_json::from_str(crate::text(&row, "data")?).map_err(|_| Error::Invalid)?;
        if grant.owner != owner(&grant.issuer, &grant.subject) {
            return Err(Error::Invalid);
        }
        owners.push(grant.owner);
    }
    for identity in owners {
        if identity == IMPORTED || identity.is_empty() {
            return Err(Error::Invalid);
        }
        if tx.get(LEGACY, &[identity.clone().into()])?.is_none() {
            tx.insert(LEGACY, crate::row([("identity", identity.into())]))?;
        }
    }
    tx.insert(LEGACY, crate::row([("identity", IMPORTED.into())]))
}

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
    /// Set only by `start_link` after fresh current-session proof.
    #[serde(default)]
    pub target: Option<String>,
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
pub struct Grant {
    /// Unpadded base64url encoding of the local bearer's raw SHA-256 digest, not
    /// an upstream token. The backing Identity session uses the decoded bytes.
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

fn attempt_id(state: &str) -> String {
    format!("oauth:{}", digest(state))
}

pub fn start(tx: &mut Transaction<'_>, state: &str, attempt: &Attempt) -> Result<(), Error> {
    if state.len() < 32
        || attempt.binding.is_empty()
        || attempt.expires < 0
        || attempt.processing
        || attempt.target.is_some()
    {
        return Err(Error::Invalid);
    }
    import_legacy_owners(tx, &[])?;
    write(tx, ATTEMPTS, &attempt_id(state), attempt, true)
}

/// Links only the proved issuer/subject. Profile email is never an account key.
/// The Crypto provider must derive raw SHA-256 session keys, as promised by
/// `Crypto::digest`, to interoperate with OAuth's persisted grant identifier.
pub fn start_link(
    tx: &mut Transaction<'_>,
    crypto: &impl crate::Crypto,
    bearer: &str,
    state: &str,
    attempt: &Attempt,
    now: i64,
) -> Result<(), Error> {
    let principal = crate::Identity::default().fresh(tx, crypto, bearer, now)?;
    start(tx, state, attempt)?;
    let mut attempt = attempt.clone();
    attempt.target = Some(principal.identity);
    attempt.old_session = Some(digest(bearer));
    write(tx, ATTEMPTS, &attempt_id(state), &attempt, false)
}

fn locator(issuer: &str, subject: &str) -> Result<String, Error> {
    if issuer.is_empty()
        || issuer.len() > 2048
        || issuer.contains('\n')
        || subject.is_empty()
        || subject.len() > 255
        || !subject.is_ascii()
        || subject.contains('\n')
    {
        return Err(Error::Invalid);
    }
    Ok(format!("oauth:{}", owner(issuer, subject)))
}
fn session_digest(id: &str) -> Result<alloc::vec::Vec<u8>, Error> {
    let digest = URL_SAFE_NO_PAD.decode(id).map_err(|_| Error::Invalid)?;
    if digest.len() != 32 {
        return Err(Error::Invalid);
    }
    Ok(digest)
}
fn authority(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<crate::Principal, Error> {
    crate::session::Sessions::resolve_digest(tx, &session_digest(id)?, now)
}
fn authorized(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<Grant, Error> {
    let principal = authority(tx, id, now)?;
    let grant: Grant = read(tx, SESSIONS, id)?.ok_or(Error::NotFound)?;
    if grant.owner != principal.identity {
        return Err(Error::NotFound);
    }
    Ok(grant)
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
    let id = attempt_id(state);
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
    session: &Grant,
    now: i64,
) -> Result<Grant, Error> {
    let attempt: Attempt = read(tx, ATTEMPTS, &attempt_id(state))?.ok_or(Error::NotFound)?;
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
    let locator = locator(&session.issuer, &session.subject)?;
    let credential = crate::Credential::lookup(tx, &locator)?;
    if credential
        .as_ref()
        .is_some_and(|c| c.kind() != crate::CredentialKind::OAuth)
    {
        return Err(Error::Invalid);
    }
    let owner = if let Some(target) = &attempt.target {
        if tx.get(LEGACY, &[session.owner.clone().into()])?.is_some() && target != &session.owner {
            return Err(Error::Constraint);
        }
        let old = attempt.old_session.as_deref().ok_or(Error::NotFound)?;
        let principal = authority(tx, old, now)?;
        if &principal.identity != target
            || principal.authenticated_at <= 0
            || now.saturating_sub(principal.authenticated_at) >= 300
        {
            return Err(Error::NotFound);
        }
        if credential.as_ref().is_some_and(|c| c.identity() != target) {
            return Err(Error::Constraint);
        }
        target.clone()
    } else {
        credential
            .as_ref()
            .map(|c| String::from(c.identity()))
            .unwrap_or_else(|| session.owner.clone())
    };
    if credential.is_none() {
        let exists = tx.get(crate::TABLES[0], &[owner.clone().into()])?.is_some();
        // A removed credential must not recreate access to its former account.
        // Linking with fresh proof may restore it; anonymous re-enrollment may not.
        if exists && attempt.target.is_none() {
            return Err(Error::Constraint);
        }
        if !exists {
            tx.insert(crate::TABLES[0], crate::row([("id", owner.clone().into())]))?;
        }
        let material = serde_json::to_string(&(session.issuer.clone(), session.subject.clone()))
            .map_err(|_| Error::Invalid)?;
        crate::Credential::insert(
            tx,
            &locator,
            &owner,
            crate::CredentialKind::OAuth,
            &material,
            &session.issuer,
        )?;
        tx.delete(LEGACY, &[session.owner.clone().into()])?;
    }
    if attempt.target.is_none() {
        if let Some(old) = attempt.old_session {
            revoke_id(tx, &old)?;
        }
    }
    let mut session = session.clone();
    session.owner = owner;
    // Silent upstream authorization cannot invent fresh user authentication.
    // Missing auth_time records unknown freshness, like migrated sessions.
    let authenticated_at = session.tokens.auth_time.unwrap_or(0);
    if authenticated_at < 0 || authenticated_at > now {
        return Err(Error::Invalid);
    }
    crate::session::Sessions::insert(
        tx,
        session_digest(&session.id)?,
        &session.owner,
        authenticated_at,
        session.expires,
    )?;
    write(tx, SESSIONS, &session.id, &session, true)?;
    tx.delete(ATTEMPTS, &[attempt_id(state).into()])?;
    Ok(session)
}

/// A refreshing session has no usable authority. Hosts arrange an explicit refresh
/// before accepting protected commands; a failed exchange requires a fresh login.
pub fn resolve(tx: &mut Transaction<'_>, bearer: &str, now: i64) -> Result<Grant, Error> {
    resolve_id(tx, &digest(bearer), now)
}
pub fn resolve_id(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<Grant, Error> {
    let session: Grant = authorized(tx, id, now)?;
    if session.expires <= now || session.tokens.access_expires <= now || session.refreshing {
        return Err(Error::NotFound);
    }
    Ok(session)
}

/// Authority for already accepted work and connected Document operations may
/// survive an in-flight refresh while the old access token remains valid. Local
/// logout, access expiry or failed refresh immediately fences further publication.
pub fn lease(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<Grant, Error> {
    let session: Grant = authorized(tx, id, now)?;
    if session.expires <= now || session.tokens.access_expires <= now {
        return Err(Error::NotFound);
    }
    Ok(session)
}

/// Preserve recovery state through access expiry and an owned refresh, not
/// application authority. A lost/uncertain rotation remains fenced by `resolve`
/// and `lease`, and startup recovery deletes it. Failure/logout deletes the
/// session, so the next lifetime check retires its state without replaying IO.
pub fn retained(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<Grant, Error> {
    let session: Grant = authorized(tx, id, now)?;
    if session.expires <= now {
        return Err(Error::NotFound);
    }
    Ok(session)
}

/// Run once before opening listeners. Lost exchanges are not replayed and cannot
/// retain authority through an old access token after restart.
pub fn recover(tx: &mut Transaction<'_>, now: i64) -> Result<(), Error> {
    import_legacy_owners(tx, &[])?;
    for row in tx.find(SESSIONS, "primary", &[])? {
        let Some(snap_store::Value::Text(id)) = row.get("id") else {
            return Err(Error::Invalid);
        };
        let session: Grant = read(tx, SESSIONS, id)?.ok_or(Error::Invalid)?;
        if session.refreshing
            || session.expires <= now
            || matches!(authority(tx, id, now), Err(Error::NotFound))
        {
            revoke_id(tx, id)?;
        }
    }
    for row in tx.find(ATTEMPTS, "primary", &[])? {
        let Some(snap_store::Value::Text(id)) = row.get("id") else {
            return Err(Error::Invalid);
        };
        if !id.starts_with("oauth:") {
            continue;
        }
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
) -> Result<Option<Grant>, Error> {
    begin_refresh_id(tx, &digest(bearer), now)
}

/// Native hosts can refresh a session selected by an application credential.
/// The host must validate that credential before resolving this private ID.
pub fn begin_refresh_id(
    tx: &mut Transaction<'_>,
    id: &str,
    now: i64,
) -> Result<Option<Grant>, Error> {
    let mut session: Grant = authorized(tx, id, now)?;
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
    previous: &Grant,
    tokens: Tokens,
    now: i64,
) -> Result<Grant, Error> {
    let mut session: Grant = authorized(tx, &previous.id, now)?;
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
    tx.delete(
        crate::TABLES[2],
        &[snap_store::Value::Bytes(session_digest(id)?)],
    )?;
    tx.delete(SESSIONS, &[id.into()])?;
    Ok(())
}
/// Local logout may revoke an expired upstream grant or an uncertain refresh.
/// This method is not authority for application reads or writes.
pub fn for_logout(tx: &mut Transaction<'_>, bearer: &str, now: i64) -> Result<Grant, Error> {
    let session: Grant = authorized(tx, &digest(bearer), now)?;
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
        if !id.starts_with("oauth:") {
            continue;
        }
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
    tx.delete(ATTEMPTS, &[attempt_id(state).into()])?;
    Ok(())
}

/// Validate already signature-verified claims. The native adapter must pin RS256,
/// fetch JWKS only from the configured issuer, and reject redirects on token IO.
pub struct Validation<'a> {
    pub issuer: &'a str,
    pub client: &'a str,
    pub nonce: Option<&'a str>,
    pub previous: Option<&'a Grant>,
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
        || claims
            .get("auth_time")
            .is_some_and(|t| t.as_i64().is_none_or(|t| t < 0 || t > now))
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
