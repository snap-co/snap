//! Authy profiles and account metadata. Identity owns authentication; callers
//! supply public actor and authentication facts. Profile initialization can share
//! enrollment's transaction without changing Identity's operations or contracts.
#![no_std]
extern crate alloc;

pub mod client;
pub mod operations;

use alloc::{
    borrow::ToOwned,
    format,
    string::{String, ToString},
};
use snap_access::{Audience, Role};
use snap_document::{Definition, Intent, Mutation, Registry, Snapshot};
use snap_store::{Error as StoreError, Row, Transaction, Value};

/// App profile kind selected in `Snapshot::kind` (independent of the Access
/// resource kind `"document"` used for every document resource).
pub const PROFILE_KIND: &str = "authy-profile";
/// Compatible deterministic profile behavior and schema version.
pub const PROFILE_VERSION: &str = "1";
/// Sole profile mutation name.
pub const PROFILE_MUTATION: &str = "edit";
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

impl Account {
    pub fn data() -> snap_store::Data {
        snap_store::Data::new(&TABLES)
    }
}
/// Application-owned profile and metadata interface used by enrollment composition.
pub fn enrollment_data() -> snap_store::Data {
    Account::data().and(document().data())
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
    snap_document::server::Document::new(registry())
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

/// Enrollment hook for Identity's operation dispatcher. This shares the
/// transaction that issued the credential and session; failure rolls all back.
/// `email` is the canonical locator supplied by Identity. The derived profile is
/// private to its owner; collisions reject enrollment without overwriting data.
pub fn initialize_account(
    tx: &mut Transaction<'_>,
    identity: &str,
    email: &str,
) -> Result<(), StoreError> {
    let profile = profile_id(identity).ok_or(StoreError::Invalid)?;
    let normalized = email.to_owned();
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
    document().create(tx, &snapshot, Audience::Restricted, identity)?;
    let mut row = Row::new();
    row.insert("identity".into(), Value::Text(identity.into()));
    row.insert("profile".into(), Value::Text(profile));
    row.insert("email".into(), Value::Text(normalized));
    tx.insert(ACCOUNTS, row)?;
    Ok(())
}

/// Join account metadata to authentication facts supplied by the bearer provider.
/// The timestamp is the captured authentication time, never the current clock.
/// Unknown identities report `NotFound`; cold metadata remains a Store miss.
pub fn account_by_identity(
    tx: &mut Transaction<'_>,
    identity: &str,
    authenticated_at: i64,
) -> Result<Account, StoreError> {
    if identity.is_empty() || authenticated_at < 0 {
        return Err(StoreError::Invalid);
    }
    let row = tx
        .get(ACCOUNTS, &[Value::Text(identity.into())])?
        .ok_or(StoreError::NotFound)?;
    Ok(Account {
        identity: identity.into(),
        email: text_field(&row, "email")?.to_string(),
        profile: text_field(&row, "profile")?.to_string(),
        authenticated_at,
    })
}

/// Look up private profile metadata by identity without an auth timestamp.
/// Unknown identities report `NotFound`. Hosts that need a full [`Account`]
/// with `auth_time` should use [`account_by_identity`] with captured authentication
/// facts instead.
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

/// Captured pre-change ownership + freshness check: dispatch supplies the
/// verified actor's effective `Owner` role, and `args.revision` must equal the
/// current snapshot revision. Runs against pre-change Access state alongside the
/// mutation's `Owner` minimum, so staged grants in the same change cannot
/// authorize themselves. Stale concurrent edits report `Denied` without a
/// document write, while Document itself keeps its latest-state policy.
fn guard_owner(
    _tx: &mut Transaction<'_>,
    snapshot: &Snapshot,
    intent: &Intent,
    actor: &str,
    role: Role,
) -> Result<bool, StoreError> {
    if actor.is_empty() || role != Role::Owner {
        return Ok(false);
    }
    let Some(revision) = intent.args.get("revision").and_then(|v| v.as_u64()) else {
        return Ok(false);
    };
    Ok(revision == snapshot.revision)
}

fn text_field<'a>(row: &'a Row, column: &str) -> Result<&'a str, StoreError> {
    match row.get(column) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(StoreError::Invalid),
    }
}
