//! Credential locators, enrollment and proof verification. Hashes stay private.
use crate::{Crypto, TABLES, email_key, hex, row, text};
use alloc::{string::String, vec::Vec};
use snap_store::{Data, Error, Transaction};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSummary {
    pub label: String,
    pub kind: CredentialKind,
    pub removable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialKind {
    Password,
}

pub struct Credential {
    email: String,
    identity: String,
    hash: String,
}
impl Credential {
    pub fn data() -> Data {
        Data::new(&[TABLES[0], TABLES[1]])
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn locator(&self) -> &str {
        &self.email
    }
    /// Unknown locators are absence; nonresident locators remain Store misses.
    pub fn find(tx: &mut Transaction<'_>, email: &str) -> Result<Option<Self>, Error> {
        let email = email_key(email)?;
        tx.get(TABLES[1], &[email.clone().into()])?
            .map(|row| {
                Ok(Self {
                    email,
                    identity: text(&row, "identity")?.into(),
                    hash: text(&row, "hash")?.into(),
                })
            })
            .transpose()
    }
    pub fn validate(&self, crypto: &impl Crypto, password: &str) -> Result<(), Error> {
        password_input(password)?;
        if crypto.verify_password(password, &self.hash)? {
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
        tx.insert(
            TABLES[1],
            row([
                ("email", email.clone().into()),
                ("identity", identity.clone().into()),
                ("hash", hash.clone().into()),
            ]),
        )?;
        Ok(Self {
            email,
            identity,
            hash,
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
                    label: text(&row, "email")?.into(),
                    kind: CredentialKind::Password,
                    removable: false,
                });
            }
        }
        Ok(labels)
    }
}

pub(crate) fn password_input(password: &str) -> Result<(), Error> {
    if (8..=1024).contains(&password.len()) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
