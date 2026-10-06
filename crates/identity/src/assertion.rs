//! Redirect-free SSO with short-lived, audience-bound signed bearer assertions.
//! Hosts pin issuer/JWKS out of band. No token-controlled URL is fetched here.
use crate::{Credential, CredentialKind, Crypto, Identity, Issued, oauth, row, session::Sessions};
use alloc::{format, string::String};
use snap_store::{Data, Error, Transaction};
use snap_transport::bearer::Receiver;

pub const PURPOSE: &str = "snap.agent-login";
pub const MAX_LIFETIME: i64 = 300;

pub fn data() -> Data {
    Identity::default().data()
}

/// Reusable until expiry, not a one-time authorization code. Local sessions never
/// outlive the assertion, bounding upstream key revocation to five minutes.
/// The issuer's subject maps identically to browser OAuth, never to its sponsor.
pub fn acquire(
    tx: &mut Transaction<'_>,
    crypto: &mut impl Crypto,
    token: &str,
    issuer: &str,
    audience: &str,
    jwks: &serde_json::Value,
    now: i64,
) -> Result<Issued, Error> {
    if token.is_empty() || token.len() > 8192 || now < 0 {
        return Err(Error::Invalid);
    }
    let claims = crypto.verify_token(token, jwks)?;
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 255 && s.is_ascii() && !s.contains('\n'))
        .ok_or(Error::Invalid)?;
    let issued = claims["iat"].as_i64().ok_or(Error::Invalid)?;
    let expires = claims["exp"].as_i64().ok_or(Error::Invalid)?;
    if claims["iss"].as_str() != Some(issuer)
        || issuer.is_empty()
        || claims["aud"].as_str() != Some(audience)
        || audience.is_empty()
        || claims["purpose"].as_str() != Some(PURPOSE)
        || issued < 0
        || issued > now
        || expires <= now
        || expires <= issued
        || expires.saturating_sub(issued) > MAX_LIFETIME
    {
        return Err(Error::NotFound);
    }
    let owner = oauth::owner(issuer, subject);
    let locator = format!("oauth:{owner}");
    let credential = Credential::lookup(tx, &locator)?;
    let identity: String = if let Some(credential) = credential {
        if credential.kind() != CredentialKind::OAuth {
            return Err(Error::NotFound);
        }
        credential.identity().into()
    } else {
        // Preserve OAuth's removed-credential fence rather than silently relinking.
        if tx.get(crate::TABLES[0], &[owner.clone().into()])?.is_some() {
            return Err(Error::Constraint);
        }
        tx.insert(crate::TABLES[0], row([("id", owner.clone().into())]))?;
        Credential::insert(
            tx,
            &locator,
            &owner,
            CredentialKind::OAuth,
            &serde_json::to_string(&(issuer, subject)).map_err(|_| Error::Invalid)?,
            issuer,
        )?;
        owner
    };
    Sessions::issue(tx, crypto, &identity, now, expires - now)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub assertion: String,
}
pub struct Acquire;
impl snap_transport::Operation for Acquire {
    const NAME: &'static str = "identity.assertion-acquire";
    type Input = Proof;
    type Output = crate::Principal;
    type Error = ();
    type Progress = ();
}
pub fn operation<C: Crypto>(
    issuer: String,
    audience: String,
    jwks: serde_json::Value,
    crypto: impl Fn() -> C + Send + 'static,
) -> snap_transport::operation::Definition {
    snap_transport::operation::Definition::typed::<Acquire>(
        false,
        alloc::vec![],
        data(),
        &["clock"],
        move |tx, proof, context| {
            let now = context
                .inputs
                .get("clock")
                .and_then(serde_json::Value::as_i64)
                .ok_or(Error::Unavailable)?;
            let issued = acquire(
                tx,
                &mut crypto(),
                &proof.assertion,
                &issuer,
                &audience,
                &jwks,
                now,
            )?;
            context
                .bearer_changed(snap_transport::bearer::Change::Set(
                    snap_transport::bearer::Token::new(issued.bearer),
                ))
                .map_err(|_| Error::Invalid)?;
            Ok(issued.principal)
        },
    )
}
