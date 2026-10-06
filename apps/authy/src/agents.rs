//! Authy declares independent accounts for sponsored agents. Keys and ownership
//! are private; neither is an Access grant to an application's resources.
use alloc::{format, string::String, vec, vec::Vec};
use snap_identity::{Crypto, Identity, oauth};
use snap_store::{Data, Error, Row, Transaction};
use snap_transport::{
    Operation,
    operation::{Context, Definition},
};

pub const TABLE: &str = "authy.agents";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInput {
    pub name: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInput {
    pub identity: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub identity: String,
    pub name: String,
    pub active: bool,
}
// Intentionally no Debug. This is the only management output containing a key.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Key {
    pub identity: String,
    pub key: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginInput {
    pub identity: String,
    pub key: String,
    pub audience: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Assertion {
    pub assertion: String,
    pub subject: String,
    pub expires: i64,
}
macro_rules! operation {
    ($kind:ident, $name:literal, $input:ty, $output:ty) => {
        pub struct $kind;
        impl Operation for $kind {
            const NAME: &'static str = $name;
            type Input = $input;
            type Output = $output;
            type Error = ();
            type Progress = ();
        }
    };
}
operation!(Create, "authy.agent-create", CreateInput, Key);
operation!(List, "authy.agents", (), Vec<Agent>);
operation!(Rotate, "authy.agent-rotate", AgentInput, Key);
operation!(Revoke, "authy.agent-revoke", AgentInput, ());
operation!(Login, "authy.agent-login", LoginInput, Assertion);
pub fn data() -> Data {
    crate::enrollment_data().and(Identity::default().data())
}
fn actor(context: &Context) -> Result<&str, Error> {
    context.actor.as_deref().ok_or(Error::NotFound)
}
fn managed(tx: &mut Transaction<'_>, actor: &str, id: &str) -> Result<Row, Error> {
    let row = tx.get(TABLE, &[id.into()])?.ok_or(Error::NotFound)?;
    if crate::text(&row, "owner")? != actor {
        return Err(Error::NotFound);
    }
    Ok(row)
}
fn human(tx: &mut Transaction<'_>, actor: &str) -> Result<(), Error> {
    crate::account_by_identity(tx, actor, 0)?;
    if tx.get(TABLE, &[actor.into()])?.is_some() {
        return Err(Error::NotFound);
    }
    Ok(())
}
fn new_key(crypto: &mut impl Crypto) -> Result<String, Error> {
    Ok(crypto
        .random()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub fn create(
    tx: &mut Transaction<'_>,
    crypto: &mut impl Crypto,
    owner: &str,
    name: &str,
) -> Result<Key, Error> {
    human(tx, owner)?;
    let (name, _) = crate::normalize_names(name, "").map_err(|_| Error::Invalid)?;
    let identity = Identity::default().declare(tx, crypto)?;
    crate::initialize_account(tx, &identity, "")?;
    let profile = crate::profile_id(&identity).ok_or(Error::Invalid)?;
    tx.update(
        crate::PROFILES,
        &[profile.into()],
        Row::from([("first_name".into(), name.into())]),
    )?;
    let key = new_key(crypto)?;
    tx.insert(
        TABLE,
        Row::from([
            ("identity".into(), identity.clone().into()),
            ("owner".into(), owner.into()),
            ("key_digest".into(), oauth::digest(&key).into()),
            ("active".into(), 1.into()),
        ]),
    )?;
    Ok(Key { identity, key })
}
pub fn verify(tx: &mut Transaction<'_>, identity: &str, key: &str) -> Result<(), Error> {
    let row = tx.get(TABLE, &[identity.into()])?.ok_or(Error::NotFound)?;
    if key.len() != 64
        || !key.bytes().all(|c| c.is_ascii_hexdigit())
        || row.get("active") != Some(&1.into())
        || !oauth::same_secret(crate::text(&row, "key_digest")?, &oauth::digest(key))
    {
        return Err(Error::NotFound);
    }
    Ok(())
}
pub fn definitions<C: Crypto>(crypto: impl Fn() -> C + Clone + Send + 'static) -> Vec<Definition> {
    let create_crypto = crypto.clone();
    vec![
        Definition::typed::<Create>(true, vec![], data(), &[], move |tx, input, context| {
            Ok(create(
                tx,
                &mut create_crypto(),
                actor(context)?,
                &input.name,
            )?)
        }),
        Definition::typed::<List>(true, vec![], data(), &[], |tx, _, context| {
            let actor = actor(context)?;
            human(tx, actor)?;
            tx.find(TABLE, "owner", &[actor.into()])?
                .into_iter()
                .map(|row| {
                    let identity: String = crate::text(&row, "identity")?.into();
                    let profile = crate::Profile::read(
                        tx,
                        &crate::profile_id(&identity).ok_or(Error::Invalid)?,
                    )?;
                    Ok(Agent {
                        identity,
                        name: profile.display_name(),
                        active: row.get("active") == Some(&1.into()),
                    })
                })
                .collect::<Result<Vec<_>, Error>>()
                .map_err(Into::into)
        }),
        Definition::typed::<Rotate>(true, vec![], data(), &[], move |tx, input, context| {
            managed(tx, actor(context)?, &input.identity)?;
            let key = new_key(&mut crypto())?;
            tx.update(
                TABLE,
                &[input.identity.clone().into()],
                Row::from([
                    ("key_digest".into(), oauth::digest(&key).into()),
                    ("active".into(), 1.into()),
                ]),
            )?;
            Ok(Key {
                identity: input.identity,
                key,
            })
        }),
        Definition::typed::<Revoke>(true, vec![], data(), &[], |tx, input, context| {
            managed(tx, actor(context)?, &input.identity)?;
            tx.update(
                TABLE,
                &[input.identity.into()],
                Row::from([
                    ("active".into(), 0.into()),
                    ("key_digest".into(), "".into()),
                ]),
            )?;
            Ok(())
        }),
    ]
}

/// Host supplies pure signing with its existing OIDC key and registered audiences.
pub fn login(
    issuer: String,
    audiences: Vec<String>,
    sign: impl Fn(&serde_json::Value) -> Result<String, Error> + Send + 'static,
) -> Definition {
    Definition::typed::<Login>(
        false,
        vec![],
        data(),
        &["clock"],
        move |tx, input, context| {
            if !audiences.contains(&input.audience) {
                return Err(Error::NotFound.into());
            }
            verify(tx, &input.identity, &input.key)?;
            let now = context
                .inputs
                .get("clock")
                .and_then(serde_json::Value::as_i64)
                .ok_or(Error::Unavailable)?;
            let expires = now
                .checked_add(snap_identity::assertion::MAX_LIFETIME)
                .ok_or(Error::Invalid)?;
            let assertion = sign(
                &serde_json::json!({"iss":issuer,"sub":input.identity,"aud":input.audience,
            "purpose":snap_identity::assertion::PURPOSE,"iat":now,"exp":expires}),
            )?;
            Ok(Assertion {
                assertion,
                subject: input.identity,
                expires,
            })
        },
    )
}
