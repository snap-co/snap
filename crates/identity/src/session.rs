//! Private session storage. Callers receive authentication facts or explicit
//! management views, never session rows, digests or storage identifiers.
use crate::{Crypto, Issued, Principal, TABLES, hex, row, session_id, text};
use alloc::{string::String, vec::Vec};
use snap_store::{Data, Error, Transaction, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseScope {
    Current,
    Others,
    All,
}
impl ReleaseScope {
    pub fn parse(scope: &str) -> Result<Self, Error> {
        match scope {
            "current" => Ok(Self::Current),
            "others" => Ok(Self::Others),
            "all" => Ok(Self::All),
            _ => Err(Error::Invalid),
        }
    }
}
/// Explicit account-management view. The opaque id cannot authenticate.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub id: String,
    pub expires: i64,
    pub current: bool,
}
struct Session {
    identity: String,
    issued: i64,
}
impl Session {
    fn principal(self) -> Principal {
        Principal {
            identity: self.identity,
            authenticated_at: self.issued,
        }
    }
}
pub(crate) struct Sessions;
impl Sessions {
    pub fn data() -> Data {
        Data::new(&[TABLES[2]])
    }
    pub fn issue(
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        identity: &str,
        now: i64,
        lifetime: i64,
    ) -> Result<Issued, Error> {
        if now < 0 {
            return Err(Error::Invalid);
        }
        let expires = now.checked_add(lifetime).ok_or(Error::Invalid)?;
        let bearer = hex(&crypto.random()?);
        Self::insert(tx, crypto.digest(&bearer), identity, now, expires)?;
        Ok(Issued {
            bearer,
            principal: Principal {
                identity: identity.into(),
                authenticated_at: now,
            },
        })
    }
    pub(crate) fn insert(
        tx: &mut Transaction<'_>,
        digest: Vec<u8>,
        identity: &str,
        now: i64,
        expires: i64,
    ) -> Result<(), Error> {
        if now < 0 || expires <= now {
            return Err(Error::Invalid);
        }
        tx.insert(
            TABLES[2],
            row([
                ("digest", Value::Bytes(digest)),
                ("identity", identity.into()),
                ("expires", expires.into()),
                ("issued", now.into()),
            ]),
        )
    }
    pub fn resolve(
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        bearer: &str,
        now: i64,
    ) -> Result<Principal, Error> {
        if now < 0 {
            return Err(Error::Invalid);
        }
        if !(bearer.len() == 64 && bearer.bytes().all(|c| c.is_ascii_hexdigit()))
            && !(bearer.len() == 43
                && bearer
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)))
        {
            return Err(Error::NotFound);
        }
        Self::resolve_digest(tx, &crypto.digest(bearer), now)
    }
    pub fn resolve_digest(
        tx: &mut Transaction<'_>,
        digest: &[u8],
        now: i64,
    ) -> Result<Principal, Error> {
        if now < 0 || digest.is_empty() {
            return Err(Error::Invalid);
        }
        let row = tx
            .get(TABLES[2], &[Value::Bytes(digest.into())])?
            .ok_or(Error::NotFound)?;
        let Some(Value::Integer(expires)) = row.get("expires") else {
            return Err(Error::Invalid);
        };
        if now >= *expires {
            return Err(Error::NotFound);
        }
        let Some(Value::Integer(issued)) = row.get("issued") else {
            return Err(Error::Invalid);
        };
        if *issued < 0 || *issued >= *expires {
            return Err(Error::Invalid);
        }
        Ok(Session {
            identity: text(&row, "identity")?.into(),
            issued: *issued,
        }
        .principal())
    }
    pub fn summaries(
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        actor: &str,
        current: &[u8],
        now: i64,
    ) -> Result<Vec<SessionSummary>, Error> {
        let mut result = Vec::new();
        for row in tx.find(TABLES[2], "primary", &[])? {
            if text(&row, "identity")? != actor {
                continue;
            }
            let (Some(Value::Bytes(digest)), Some(Value::Integer(expires))) =
                (row.get("digest"), row.get("expires"))
            else {
                return Err(Error::Invalid);
            };
            if *expires > now {
                result.push(SessionSummary {
                    id: session_id(crypto, digest),
                    expires: *expires,
                    current: digest == current,
                });
            }
        }
        Ok(result)
    }
    /// Authority was captured at admission. Do not reevaluate expiry here.
    pub fn release(
        tx: &mut Transaction<'_>,
        actor: &str,
        current: &[u8],
        scope: ReleaseScope,
    ) -> Result<(), Error> {
        for row in tx.find(TABLES[2], "primary", &[])? {
            if text(&row, "identity")? != actor {
                continue;
            }
            let Some(Value::Bytes(digest)) = row.get("digest") else {
                return Err(Error::Invalid);
            };
            if scope == ReleaseScope::All
                || (scope == ReleaseScope::Current && digest == current)
                || (scope == ReleaseScope::Others && digest != current)
            {
                tx.delete(TABLES[2], &[Value::Bytes(digest.clone())])?;
            }
        }
        Ok(())
    }
    pub fn release_one(
        tx: &mut Transaction<'_>,
        crypto: &impl Crypto,
        actor: &str,
        id: &str,
    ) -> Result<(), Error> {
        for row in tx.find(TABLES[2], "primary", &[])? {
            if text(&row, "identity")? != actor {
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
