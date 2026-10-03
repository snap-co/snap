//! Credential locators, enrollment and proof verification. Hashes stay private.
//! A locator's family is recorded in its own row, so credentials of different
//! kinds share one table and stay individually filterable.
use crate::{Crypto, TABLES, email_key, hex, row, text};
use alloc::{string::String, vec::Vec};
use snap_store::{Data, Error, Transaction};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSummary {
    pub locator: String,
    pub label: String,
    pub kind: CredentialKind,
    pub removable: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialKind {
    Password,
    Passkey,
    OAuth,
}
impl CredentialKind {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "password" => Ok(Self::Password),
            "passkey" => Ok(Self::Passkey),
            "oauth" => Ok(Self::OAuth),
            _ => Err(Error::Invalid),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passkey => "passkey",
            Self::OAuth => "oauth",
        }
    }
}

pub struct Credential {
    locator: String,
    identity: String,
    material: String,
    kind: CredentialKind,
}
impl Credential {
    /// Canonical password locator for host enrollment metadata. A matching email
    /// on another credential kind is never proof of ownership.
    pub fn canonical_email(email: &str) -> Result<String, Error> {
        email_key(email)
    }
    pub fn data() -> Data {
        Data::new(&[TABLES[0], TABLES[1]])
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn locator(&self) -> &str {
        &self.locator
    }
    /// The locator family this credential proves. Callers filter on it rather
    /// than inferring a kind from the locator's shape.
    pub fn kind(&self) -> CredentialKind {
        self.kind
    }
    /// Unknown locators are absence; nonresident locators remain Store misses.
    pub fn find(tx: &mut Transaction<'_>, email: &str) -> Result<Option<Self>, Error> {
        let email = email_key(email)?;
        Self::lookup(tx, &email)
    }
    /// Trusted server-side lookup. Material is never part of a management view.
    pub(crate) fn lookup(tx: &mut Transaction<'_>, locator: &str) -> Result<Option<Self>, Error> {
        tx.get(TABLES[1], &[locator.into()])?
            .map(|row| {
                Ok(Self {
                    locator: locator.into(),
                    identity: text(&row, "identity")?.into(),
                    material: text(&row, "material")?.into(),
                    kind: CredentialKind::parse(text(&row, "kind")?)?,
                })
            })
            .transpose()
    }
    pub fn validate(&self, crypto: &impl Crypto, password: &str) -> Result<(), Error> {
        password_input(password)?;
        if self.kind != CredentialKind::Password {
            return Err(Error::NotFound);
        }
        if crypto.verify_password(password, &self.material)? {
            Ok(())
        } else {
            Err(Error::NotFound)
        }
    }
    pub fn enroll(
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        email: &str,
        password: &str,
    ) -> Result<Self, Error> {
        let email = email_key(email)?;
        password_input(password)?;
        if Self::find(tx, &email)?.is_some() {
            return Err(Error::Constraint);
        }
        let identity = hex(&crypto.random()?);
        let hash = crypto.hash_password(password)?;
        tx.insert(TABLES[0], row([("id", identity.clone().into())]))?;
        Self::insert(
            tx,
            &email,
            &identity,
            CredentialKind::Password,
            &hash,
            &email,
        )?;
        Ok(Self {
            locator: email,
            identity,
            material: hash,
            kind: CredentialKind::Password,
        })
    }
    pub(crate) fn summaries(
        tx: &mut Transaction<'_>,
        identity: &str,
    ) -> Result<Vec<CredentialSummary>, Error> {
        let mut labels = Vec::new();
        for row in tx.find(TABLES[1], "primary", &[])? {
            if text(&row, "identity")? == identity {
                labels.push(CredentialSummary {
                    locator: text(&row, "locator")?.into(),
                    label: match text(&row, "label")? {
                        "" => text(&row, "locator")?,
                        label => label,
                    }
                    .into(),
                    kind: CredentialKind::parse(text(&row, "kind")?)?,
                    removable: false,
                });
            }
        }
        let removable = labels.len() > 1;
        for label in &mut labels {
            label.removable = removable;
        }
        Ok(labels)
    }
    pub(crate) fn material(&self) -> &str {
        &self.material
    }
    pub(crate) fn insert(
        tx: &mut Transaction<'_>,
        locator: &str,
        identity: &str,
        kind: CredentialKind,
        material: &str,
        label: &str,
    ) -> Result<(), Error> {
        label_input(label)?;
        tx.insert(
            TABLES[1],
            row([
                ("locator", locator.into()),
                ("identity", identity.into()),
                ("kind", kind.as_str().into()),
                ("material", material.into()),
                ("label", label.into()),
            ]),
        )
    }
    pub(crate) fn update_material(
        tx: &mut Transaction<'_>,
        locator: &str,
        material: &str,
    ) -> Result<(), Error> {
        tx.update(
            TABLES[1],
            &[locator.into()],
            row([("material", material.into())]),
        )
    }
    fn owned(tx: &mut Transaction<'_>, identity: &str, locator: &str) -> Result<Self, Error> {
        Self::lookup(tx, locator)?
            .filter(|c| c.identity == identity)
            .ok_or(Error::NotFound)
    }
    pub(crate) fn rename(
        tx: &mut Transaction<'_>,
        identity: &str,
        locator: &str,
        label: &str,
    ) -> Result<(), Error> {
        Self::owned(tx, identity, locator)?;
        label_input(label)?;
        tx.update(TABLES[1], &[locator.into()], row([("label", label.into())]))
    }
    pub(crate) fn remove(
        tx: &mut Transaction<'_>,
        identity: &str,
        locator: &str,
    ) -> Result<(), Error> {
        Self::owned(tx, identity, locator)?;
        if Self::summaries(tx, identity)?.len() <= 1 {
            return Err(Error::Constraint);
        }
        tx.delete(TABLES[1], &[locator.into()])?;
        Ok(())
    }
    pub(crate) fn passkeys(tx: &mut Transaction<'_>, identity: &str) -> Result<Vec<Self>, Error> {
        let mut result = Vec::new();
        for summary in Self::summaries(tx, identity)? {
            if summary.kind == CredentialKind::Passkey {
                result.push(Self::lookup(tx, &summary.locator)?.ok_or(Error::Invalid)?);
            }
        }
        Ok(result)
    }
}

pub(crate) fn label_input(label: &str) -> Result<(), Error> {
    if label.is_empty() || label.len() > 254 || label.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok(())
}

pub(crate) fn password_input(password: &str) -> Result<(), Error> {
    if (8..=1024).contains(&password.len()) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
