//! Authy account metadata and owner-authorized Store profiles. Identity owns
//! authentication; profiles use ordinary Transport operations and Store programs.
#![no_std]
extern crate alloc;
pub mod client;
pub mod operations;

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use snap_access::{Access, Audience, DirectGrant, KindDefinition, Resource, Role, TransferPolicy};
use snap_store::{Catalog, Column, Data, Error, Kind, Row, Table, Transaction, Value};

pub const MIGRATION: &str = include_str!("../migrations/0001_authy.toml");
pub const PROFILE_MIGRATION: &str = include_str!("../migrations/0002_authy_profiles.toml");
pub const TABLES: [&str; 2] = ["authy.accounts", "authy.profiles"];
pub const ACCOUNTS: &str = TABLES[0];
pub const PROFILES: &str = TABLES[1];
pub const PROFILE_KIND: &str = "account-profile";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub identity: String,
    pub email: String,
    pub profile: String,
    pub authenticated_at: i64,
}
impl Account {
    pub fn data() -> Data {
        Data::new(&TABLES)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub id: String,
    pub identity: String,
    pub first_name: String,
    pub last_name: String,
    pub revision: i64,
}

pub fn profile_data() -> Data {
    Data::new(&[PROFILES]).and(Data::new(&snap_access::TABLES))
}
pub fn enrollment_data() -> Data {
    Account::data().and(profile_data())
}
pub fn access() -> Access {
    Access::new(vec![
        KindDefinition::new(PROFILE_KIND, TransferPolicy::Forbidden).expect("profile kind"),
    ])
    .expect("profile vocabulary")
}

/// Replica shape deliberately excludes server-only foreign keys and indexes.
pub fn profile_catalog() -> Catalog {
    Catalog::new(vec![Table {
        name: PROFILES.into(),
        columns: vec![
            Column {
                name: "id".into(),
                kind: Kind::Text,
            },
            Column {
                name: "identity".into(),
                kind: Kind::Text,
            },
            Column {
                name: "first_name".into(),
                kind: Kind::Text,
            },
            Column {
                name: "last_name".into(),
                kind: Kind::Text,
            },
            Column {
                name: "revision".into(),
                kind: Kind::Integer,
            },
        ],
        primary: vec!["id".into()],
        indexes: Vec::new(),
        foreign: Vec::new(),
    }])
    .expect("profile schema")
}

pub fn replication() -> Arc<snap_transport::replication::Registry> {
    Arc::new(
        snap_transport::replication::Registry::new(vec![
            snap_transport::replication::Declaration::new(
                profile_catalog().tables.remove(0),
                profile_data(),
                |tx, actor, key| {
                    let [Value::Text(id)] = key else {
                        return Err(Error::Invalid);
                    };
                    Ok(snap_access::allows(
                        access().role(tx, &Resource::new(PROFILE_KIND, id)?, Some(actor), true)?,
                        Role::Viewer,
                    ))
                },
            ),
        ])
        .expect("profile replication"),
    )
}

/// Stable profile resource id. Prefix collisions reject enrollment atomically.
pub fn profile_id(identity: &str) -> Option<String> {
    if identity.len() != 64 || !identity.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let p = identity[..32].to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &p[..8],
        &p[8..12],
        &p[12..16],
        &p[16..20],
        &p[20..]
    ))
}

pub fn initialize_account(
    tx: &mut Transaction<'_>,
    identity: &str,
    email: &str,
) -> Result<(), Error> {
    let id = profile_id(identity).ok_or(Error::Invalid)?;
    if tx.get(PROFILES, &[id.clone().into()])?.is_some()
        || !tx
            .find(ACCOUNTS, "profile", &[id.clone().into()])?
            .is_empty()
    {
        return Err(Error::Constraint);
    }
    let first_name: String = email
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .take(100)
        .collect();
    let profile = Profile {
        id: id.clone(),
        identity: identity.into(),
        first_name,
        last_name: String::new(),
        revision: 1,
    };
    tx.insert(PROFILES, profile.row())?;
    access().register(
        tx,
        &Resource::new(PROFILE_KIND, &id)?,
        Audience::Restricted,
        &[DirectGrant::new(identity, Role::Owner)?],
        None,
    )?;
    tx.insert(
        ACCOUNTS,
        Row::from([
            ("identity".into(), identity.into()),
            ("profile".into(), id.into()),
            ("email".into(), email.into()),
        ]),
    )
}

pub fn account_by_identity(
    tx: &mut Transaction<'_>,
    identity: &str,
    authenticated_at: i64,
) -> Result<Account, Error> {
    if identity.is_empty() || authenticated_at < 0 {
        return Err(Error::Invalid);
    }
    let row = tx
        .get(ACCOUNTS, &[identity.into()])?
        .ok_or(Error::NotFound)?;
    Ok(Account {
        identity: identity.into(),
        email: text(&row, "email")?.into(),
        profile: text(&row, "profile")?.into(),
        authenticated_at,
    })
}

pub struct ProfileInfo {
    pub email: String,
    pub profile: String,
}
pub fn profile_info(tx: &mut Transaction<'_>, identity: &str) -> Result<ProfileInfo, Error> {
    let account = account_by_identity(tx, identity, 0)?;
    Ok(ProfileInfo {
        email: account.email,
        profile: account.profile,
    })
}

fn text<'a>(row: &'a Row, name: &str) -> Result<&'a str, Error> {
    match row.get(name) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(Error::Invalid),
    }
}
impl Profile {
    pub fn from_row(row: &Row) -> Result<Self, Error> {
        let Some(Value::Integer(revision)) = row.get("revision") else {
            return Err(Error::Invalid);
        };
        Ok(Self {
            id: text(row, "id")?.into(),
            identity: text(row, "identity")?.into(),
            first_name: text(row, "first_name")?.into(),
            last_name: text(row, "last_name")?.into(),
            revision: *revision,
        })
    }
    fn row(&self) -> Row {
        Row::from([
            ("id".into(), self.id.clone().into()),
            ("identity".into(), self.identity.clone().into()),
            ("first_name".into(), self.first_name.clone().into()),
            ("last_name".into(), self.last_name.clone().into()),
            ("revision".into(), self.revision.into()),
        ])
    }
    pub fn read(tx: &mut Transaction<'_>, id: &str) -> Result<Self, Error> {
        Self::from_row(&tx.get(PROFILES, &[id.into()])?.ok_or(Error::NotFound)?)
    }
    pub fn display_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
            .trim()
            .to_string()
    }
}

pub fn normalize_names(first: &str, last: &str) -> Result<(String, String), operations::EditError> {
    let first = first.trim();
    let last = last.trim();
    if first.is_empty()
        || first.chars().count() > 100
        || last.chars().count() > 100
        || first.chars().chain(last.chars()).any(char::is_control)
    {
        return Err(operations::EditError::Invalid);
    }
    Ok((first.into(), last.into()))
}
