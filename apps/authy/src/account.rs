//! Authy owns account documents; Passport owns credentials and sessions. New
//! accounts join enrollment atomically. Legacy identities receive their initial
//! document lazily under a current-session guard, without changing their ID.
use alloc::{
    string::{String, ToString},
    vec,
};
use serde_json::{Value, json};
use snap_http::Response;
use snap_oidc::{
    Identity,
    storage::{guard, integer, read, row, text, transaction},
};
use snap_runtime::passport::{Crypto, Material, Passport};
use snap_store::{Cache, Kind, Predicate as P, Query, Schema, Statement as S, Store, Table};

pub const ACCOUNTS: Table = Table {
    namespace: "authy",
    name: "accounts",
};
pub fn schema() -> Schema {
    Schema {
        table: ACCOUNTS,
        columns: &[
            ("id", Kind::Text),
            ("document", Kind::Text),
            ("revision", Kind::Integer),
            ("updated", Kind::Integer),
        ],
        primary: &["id"],
        indexes: &[],
        foreign: &[],
        legacy_name: None,
    }
}
pub fn enrollment(material: &Material, email: &str, now: u64) -> alloc::vec::Vec<S> {
    vec![insert(&material.identity, email, now)]
}
fn insert(id: &str, email: &str, now: u64) -> S {
    S::Insert {
        table: ACCOUNTS,
        row: row(&[
            ("id", id.into()),
            (
                "document",
                json!({"name":email.split('@').next().unwrap_or(""),"bio":""})
                    .to_string()
                    .into(),
            ),
            ("revision", 1i64.into()),
            ("updated", (now as i64).into()),
        ]),
    }
}
#[derive(Clone)]
pub struct Accounts<S, C, K> {
    pub store: S,
    pub passport: Passport<S, C, K>,
}
impl<D: Store, C: Crypto, K: Cache> Accounts<D, C, K> {
    async fn document(
        &self,
        subject: &str,
        email: &str,
        now: u64,
        authority: snap_store::Guard,
    ) -> Result<snap_store::Row, Response> {
        let q = Query::new(ACCOUNTS)
            .matching(vec![P::eq("id", subject)])
            .limit(1);
        if let Some(row) = read(&self.store, q.clone()).await? {
            return Ok(row);
        }
        let result = transaction(
            &self.store,
            vec![authority],
            vec![insert(subject, email, now)],
        )
        .await;
        if result.is_err() {
            return read(&self.store, q)
                .await?
                .ok_or_else(snap_oidc::unavailable);
        }
        read(&self.store, q)
            .await?
            .ok_or_else(snap_oidc::unavailable)
    }
    pub async fn resolve(&self, token: &str, now: u64) -> Result<Option<Identity>, Response> {
        let Some(session) = self
            .passport
            .resolve(token, now)
            .await
            .map_err(|_| snap_oidc::unavailable())?
        else {
            return Ok(None);
        };
        snap_oidc::Accounts::load(self, session.identity_id, session.session_id, now).await
    }
    pub async fn profile(&self, actor: Identity) -> Result<Value, Response> {
        let rows = transaction(
            &self.store,
            actor.authority,
            vec![S::Select(
                Query::new(ACCOUNTS)
                    .matching(vec![P::eq("id", actor.subject.clone())])
                    .limit(1),
            )],
        )
        .await?;
        let row = rows[0].first().ok_or_else(snap_oidc::unavailable)?;
        let mut value: Value =
            serde_json::from_str(&text(row, "document")?).map_err(|_| snap_oidc::unavailable())?;
        value["id"] = actor.subject.into();
        value["email"] = actor.claims["email"].clone();
        value["email_verified"] = false.into();
        value["revision"] = integer(row, "revision")?.into();
        value["updated_at"] = (integer(row, "updated")? / 1000).into();
        Ok(value)
    }
    pub async fn update(&self, actor: Identity, value: Value, now: u64) -> Result<Value, Response> {
        let bad = || {
            Response::error(
                400,
                "invalid_request",
                "Name must be 1–100 characters and bio at most 2000 characters; revision is required",
            )
        };
        let name = value["name"].as_str().ok_or_else(bad)?.trim();
        let bio = value["bio"].as_str().unwrap_or("").trim();
        let revision = value["revision"]
            .as_i64()
            .filter(|n| *n > 0 && *n < i64::MAX)
            .ok_or_else(bad)?;
        if name.is_empty() || name.chars().count() > 100 || bio.chars().count() > 2000 {
            return Err(bad());
        }
        let mut guards = actor.authority.clone();
        guards.push(guard(
            Query::new(ACCOUNTS)
                .matching(vec![
                    P::eq("id", actor.subject.clone()),
                    P::eq("revision", revision),
                ])
                .limit(1),
        ));
        transaction(
            &self.store,
            guards,
            vec![S::Update {
                table: ACCOUNTS,
                filter: vec![P::eq("id", actor.subject.clone())],
                changes: row(&[
                    (
                        "document",
                        json!({"name":name,"bio":bio}).to_string().into(),
                    ),
                    ("revision", (revision + 1).into()),
                    ("updated", (now as i64).into()),
                ]),
            }],
        )
        .await
        .map_err(|r| {
            if r.status == 400 {
                Response::error(409, "conflict", "Account changed; reload before saving")
            } else {
                r
            }
        })?;
        self.profile(actor).await
    }
}
impl<D: Store, C: Crypto, K: Cache> snap_oidc::Accounts for Accounts<D, C, K> {
    async fn load(
        &self,
        subject: String,
        session: String,
        now: u64,
    ) -> Result<Option<Identity>, Response> {
        let Some(description) = self
            .passport
            .describe(&subject, &session, now)
            .await
            .map_err(|_| snap_oidc::unavailable())?
        else {
            return Ok(None);
        };
        let authority = Passport::<D, C, K>::authority(&subject, &session, now);
        let record = self
            .document(&subject, &description.email, now, authority.clone())
            .await?;
        let doc: Value = serde_json::from_str(&text(&record, "document")?)
            .map_err(|_| snap_oidc::unavailable())?;
        Ok(Some(Identity {
            subject,
            session,
            auth_time: description.authenticated_at / 1000,
            claims: json!({"name":doc["name"],"email":description.email,"email_verified":false,"updated_at":integer(&record,"updated")?/1000}),
            authority: vec![authority],
        }))
    }
    async fn end_session(
        &self,
        subject: String,
        session: String,
        _now: u64,
    ) -> Result<(), Response> {
        self.passport
            .end_session(&subject, &session)
            .await
            .map_err(|_| snap_oidc::unavailable())
    }
}
