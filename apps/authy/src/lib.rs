//! Authy account application on current Identity, Access and Document.
//!
//! Portable `no_std` with `alloc`; hosts own execution, clocks, randomness and
//! external IO. All methods take the caller's `&mut Transaction` so Identity
//! enrollment, the first session, the initial profile Document, its Access
//! grant and the private account metadata commit atomically. Hosts publish
//! results only from `Store::run`'s `Committed` value; a rejected attempt
//! publishes nothing and stages nothing.
//!
//! Non-obvious guarantees promised beside this interface:
//!
//! * One caller-owned transaction composes Identity, Access, Document and the
//!   private `authy.accounts` metadata. Failed transactions discard every
//!   staged write, including the Identity credential/session rows. There is no
//!   partial signup.
//! * Identity IDs are opaque 64-hex strings issued by `snap-identity`. Profile
//!   IDs are UUIDs deterministically derived from the first 32 hex characters
//!   grouped `8-4-4-4-12`. Two identities sharing a 32-hex prefix map to the
//!   same profile; enrollment rejects the collision with `Constraint` and never
//!   overwrites the existing profile. Hosts needing independent profile IDs
//!   must not use this derivation; the stable rejection still applies.
//! * Profile documents use app kind `"authy-profile"` version `"1"` with value
//!   `{name, bio}`. The `edit` mutation takes args `{name, bio, revision}`
//!   and requires the `Owner` role plus an app-specific guard that rechecks
//!   pre-change `Owner` authority for the verified actor AND requires
//!   `args.revision` to equal the current snapshot revision. Name is trimmed
//!   `1..=100` chars, bio (trimmed) is `<=2000` chars; `revision` must be a
//!   storage-safe `1..=i64::MAX` integer. The pure apply validates the shape
//!   (including a well-formed revision) but ignores the revision value itself;
//!   only the guard enforces equality. A stale edit therefore reports a
//!   `Denied` completion without a document write; hosts surface it as a
//!   changed/denied profile edit. There is no legacy HTTP 409 wire. Document
//!   itself stays latest-state globally: no blanket stale-base rejection.
//!   Client bindings take `revision` from the Rust projected snapshot on edit,
//!   so ACK-paced consecutive profile edits remain causal. All checks run
//!   against pre-change Access state; a mutation cannot grant itself authority.
//! * Profiles are private (`Audience::Restricted`): only the owner holds a
//!   grant. Reads, mutations and manifests for other identities report denials
//!   without leaking document data. Client profiles go through the shared
//!   [`registry`] with `snap-document`'s client SDK; there is no alternate
//!   profile storage.
//! * `authy.accounts` maps `identity -> (profile, email)` for issuer claims.
//!   It is private app metadata, not a client-visible document. Email is the
//!   normalized (trimmed, lowercased) address accepted by Identity.
//! * [`Account::authenticated_at`] for [`current`] is derived as
//!   `session.expires - 30 days`. Identity stores only the absolute expiry, so
//!   this is an app-specific fixed policy valid only with the default 30-day
//!   session lifetime. Custom lifetimes would need an explicit issued-at
//!   column. [`account_by_identity`] instead reports the caller-supplied
//!   current time as `authenticated_at` for OIDC hosts resolving by identity
//!   without a bearer.
//! * Store misses (`Error::Miss`) abort the attempt and are never converted
//!   into domain errors. Hosts must load Identity, Access, Document and Authy
//!   tables before calling, then submit a new request after loading.
//! * The native OIDC adapter uses [`account_by_identity`] / [`profile_info`]
//!   for claims in the same transaction as issuer state changes.
#![no_std]
extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
};
use snap_access::{Access, Audience, KindDefinition, Role};
use snap_document::{Definition, Intent, Mutation, Registry, Snapshot};
use snap_store::{Error as StoreError, Row, Transaction, Value};

/// App profile kind selected in `Snapshot::kind` (independent of the Access
/// resource kind `"document"` used for every document resource).
pub const PROFILE_KIND: &str = "authy-profile";
/// Compatible deterministic profile behavior and schema version.
pub const PROFILE_VERSION: &str = "1";
/// Sole profile mutation name.
pub const PROFILE_MUTATION: &str = "edit";
/// Fixed session lifetime assumed by [`Account::authenticated_at`] in
/// [`current`]. Matches `snap-identity`'s default of 30 days.
pub const SESSION_LIFETIME_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Ordered migration declarations for the Authy-owned account metadata table.
/// Module migrations are NOT copied here; hosts compose this with Identity,
/// Access and Document migrations.
pub const MIGRATION: &str = include_str!("../migrations/0001_authy.toml");
/// Store tables owned by this application. Hosts load these to arrange
/// residency alongside the Identity, Access and Document tables.
pub const TABLES: [&str; 1] = ["authy.accounts"];
/// Private `identity -> (profile, email)` metadata table for issuer claims.
pub const ACCOUNTS: &str = TABLES[0];

/// Account view returned to hosts and UI. Serialized with exactly these
/// fields: `identity`, `email`, `profile`, `authenticated_at`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub identity: String,
    pub email: String,
    pub profile: String,
    pub authenticated_at: i64,
}

/// Private profile lookup without an auth timestamp, for hosts that supply
/// their own `auth_time` (for example the OIDC issuer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub email: String,
    pub profile: String,
}

/// Shared deterministic profile behavior used identically by the authority,
/// optimistic replay and remote replication.
pub fn registry() -> snap_document::Registry {
    Registry::new(alloc::vec![Definition {
        kind: PROFILE_KIND.into(),
        version: PROFILE_VERSION.into(),
        validate: validate_profile,
        mutations: alloc::vec![Mutation {
            name: PROFILE_MUTATION.into(),
            minimum: Role::Owner,
            apply: apply_edit,
            guard: Some(guard_owner),
        }],
    }])
    .unwrap()
}

/// Document server over the caller's transaction, combining the shared
/// profile [`registry`] with an Access vocabulary containing kind
/// `"document"`.
pub fn document() -> snap_document::server::Document {
    let access = Access::new(alloc::vec![KindDefinition::kind("document").unwrap()]).unwrap();
    snap_document::server::Document::new(registry(), access)
}

/// Deterministically derive the profile UUID for an opaque 64-hex identity:
/// first 32 hex characters grouped `8-4-4-4-12` (lowercased). Returns `None`
/// for malformed identities. Colliding identities (same 32-hex prefix) map to
/// the same profile; enrollment rejects the second with `Constraint`.
pub fn profile_id(identity: &str) -> Option<String> {
    if identity.len() != 64 || !identity.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let prefix = identity[..32].to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &prefix[0..8],
        &prefix[8..12],
        &prefix[12..16],
        &prefix[16..20],
        &prefix[20..32]
    ))
}

/// Atomically enroll an Identity credential, issue its first session, create
/// the initial private profile Document with an owner grant, and record the
/// private `identity -> (profile, email)` metadata row.
///
/// The initial name is the normalized email local part (truncated to 100
/// chars when a valid address has an overlong local part) with an empty bio.
/// Profiles are `Restricted`: only the new identity holds `Owner`.
/// Duplicate normalized emails report `Constraint` from Identity. A derived
/// profile that already exists (document row or metadata) reports
/// `Constraint` without overwriting. Cold tables report `Miss`. Returning
/// `Err` stages nothing observable: `Store::run` discards the scratch state.
pub fn enroll(
    tx: &mut Transaction<'_>,
    crypto: &mut impl snap_identity::Crypto,
    email: &str,
    password: &str,
    now: i64,
) -> Result<snap_identity::Issued, StoreError> {
    let issued = snap_identity::Identity::default().enroll(tx, crypto, email, password, now)?;
    let profile = profile_id(&issued.session.identity).ok_or(StoreError::Invalid)?;
    let normalized = email.trim().to_ascii_lowercase();
    let local = normalized.split('@').next().unwrap_or("");
    let name: String = if local.chars().count() > 100 {
        local.chars().take(100).collect()
    } else {
        local.into()
    };
    // Stable collision rejection before staging document/metadata writes.
    // Cold tables report Miss here rather than a silent incomplete check.
    if tx
        .get("document.documents", &[Value::Text(profile.clone())])?
        .is_some()
    {
        return Err(StoreError::Constraint);
    }
    if !tx
        .find(ACCOUNTS, "profile", &[Value::Text(profile.clone())])?
        .is_empty()
    {
        return Err(StoreError::Constraint);
    }
    let snapshot = Snapshot {
        id: profile.clone(),
        kind: PROFILE_KIND.into(),
        version: PROFILE_VERSION.into(),
        revision: 1,
        value: serde_json::json!({"name": name, "bio": ""}),
    };
    document().create(
        tx,
        &snapshot,
        Audience::Restricted,
        &issued.session.identity,
    )?;
    let mut row = Row::new();
    row.insert(
        "identity".into(),
        Value::Text(issued.session.identity.clone()),
    );
    row.insert("profile".into(), Value::Text(profile));
    row.insert("email".into(), Value::Text(normalized));
    tx.insert(ACCOUNTS, row)?;
    Ok(issued)
}

/// Resolve the bearer to its session and return the account view. The private
/// metadata supplies `email`/`profile`; `authenticated_at` is derived as
/// `expires - 30 days` under the fixed default-lifetime policy documented
/// above. Unknown/expired bearers report `NotFound`; missing metadata reports
/// `NotFound`; cold tables report `Miss`.
pub fn current(
    tx: &mut Transaction<'_>,
    crypto: &impl snap_identity::Crypto,
    bearer: &str,
    now: i64,
) -> Result<Account, StoreError> {
    let session = snap_identity::Identity::default().resolve(tx, crypto, bearer, now)?;
    let authenticated_at = session
        .expires
        .checked_sub(SESSION_LIFETIME_SECONDS)
        .ok_or(StoreError::Invalid)?;
    let row = tx
        .get(ACCOUNTS, &[Value::Text(session.identity.clone())])?
        .ok_or(StoreError::NotFound)?;
    Ok(Account {
        identity: session.identity,
        email: text_field(&row, "email")?.to_string(),
        profile: text_field(&row, "profile")?.to_string(),
        authenticated_at,
    })
}

/// Look up an account by identity for hosts without a bearer (for example the
/// OIDC issuer building ID-token claims). `now` is the host's current Unix
/// time and is reported as `authenticated_at`, since no session expiry is
/// available on this path. Unknown identities report `NotFound`.
pub fn account_by_identity(
    tx: &mut Transaction<'_>,
    identity: &str,
    now: i64,
) -> Result<Account, StoreError> {
    if identity.is_empty() || now < 0 {
        return Err(StoreError::Invalid);
    }
    let row = tx
        .get(ACCOUNTS, &[Value::Text(identity.into())])?
        .ok_or(StoreError::NotFound)?;
    Ok(Account {
        identity: identity.into(),
        email: text_field(&row, "email")?.to_string(),
        profile: text_field(&row, "profile")?.to_string(),
        authenticated_at: now,
    })
}

/// Look up private profile metadata by identity without an auth timestamp.
/// Unknown identities report `NotFound`. Hosts that need a full [`Account`]
/// with `auth_time` should use [`account_by_identity`] with their current
/// time instead.
pub fn profile_info(tx: &mut Transaction<'_>, identity: &str) -> Result<ProfileInfo, StoreError> {
    if identity.is_empty() {
        return Err(StoreError::Invalid);
    }
    let row = tx
        .get(ACCOUNTS, &[Value::Text(identity.into())])?
        .ok_or(StoreError::NotFound)?;
    Ok(ProfileInfo {
        email: text_field(&row, "email")?.to_string(),
        profile: text_field(&row, "profile")?.to_string(),
    })
}

fn validate_profile(value: &serde_json::Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    if fields.len() != 2 {
        return false;
    }
    let Some(name) = fields.get("name").and_then(|v| v.as_str()) else {
        return false;
    };
    let Some(bio) = fields.get("bio").and_then(|v| v.as_str()) else {
        return false;
    };
    let trimmed = name.trim();
    // Stored names are canonical trimmed values; reject padded storage.
    if name != trimmed {
        return false;
    }
    if trimmed.is_empty() || trimmed.chars().count() > 100 {
        return false;
    }
    if bio.chars().count() > 2000 {
        return false;
    }
    true
}

fn apply_edit(
    _state: &serde_json::Value,
    args: &serde_json::Value,
    _actor: &str,
) -> Result<serde_json::Value, snap_document::Error> {
    let Some(fields) = args.as_object() else {
        return Err(snap_document::Error::Invalid);
    };
    if fields.len() != 3 {
        return Err(snap_document::Error::Invalid);
    }
    let Some(name) = fields.get("name").and_then(|v| v.as_str()) else {
        return Err(snap_document::Error::Invalid);
    };
    let Some(bio) = fields.get("bio").and_then(|v| v.as_str()) else {
        return Err(snap_document::Error::Invalid);
    };
    // Revision is validated as well-formed input here but otherwise ignored;
    // the guard enforces equality with the current snapshot revision.
    let Some(revision) = fields.get("revision").and_then(|v| v.as_u64()) else {
        return Err(snap_document::Error::Invalid);
    };
    if revision == 0 || revision > i64::MAX as u64 {
        return Err(snap_document::Error::Invalid);
    }
    let name = name.trim();
    let bio = bio.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(snap_document::Error::Invalid);
    }
    if bio.chars().count() > 2000 {
        return Err(snap_document::Error::Invalid);
    }
    Ok(serde_json::json!({"name": name, "bio": bio}))
}

/// Pre-change ownership + freshness recheck: the verified actor must hold
/// effective `Owner` on the profile AND `args.revision` must equal the current
/// snapshot revision. Runs against pre-change Access state alongside the
/// mutation's `Owner` minimum, so staged grants in the same change cannot
/// authorize themselves. Stale concurrent edits report `Denied` without a
/// document write, while Document itself keeps its latest-state policy.
fn guard_owner(snapshot: &Snapshot, intent: &Intent, actor: &str, role: Role) -> bool {
    if actor.is_empty() || role != Role::Owner {
        return false;
    }
    let Some(revision) = intent.args.get("revision").and_then(|v| v.as_u64()) else {
        return false;
    };
    revision == snapshot.revision
}

fn text_field<'a>(row: &'a Row, column: &str) -> Result<&'a str, StoreError> {
    match row.get(column) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(StoreError::Invalid),
    }
}
