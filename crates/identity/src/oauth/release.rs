//! Local revocation commits before publishing the upstream logout continuation.
//! The return binding proves continuity only and cannot recreate local authority.
use super::{
    Attempt,
    acquisition::{Provider, now, provider, random},
};
use crate::Crypto;
use alloc::{string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_store::Error;
use snap_transport::{
    Operation,
    bearer::{Change, Receiver},
    operation::Definition,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub provider: String,
    pub continuation: String,
    pub csrf: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Continuation {
    pub endpoint: String,
    pub client: String,
    pub redirect: String,
    pub state: String,
    pub binding: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnInput {
    pub state: String,
    pub binding: String,
}
pub struct Release;
impl Operation for Release {
    const NAME: &'static str = "identity.oauth-release";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Post, true));
    type Input = Input;
    type Output = Continuation;
    type Error = ();
    type Progress = ();
}
pub struct Returned;
impl Operation for Returned {
    const NAME: &'static str = "identity.oauth-released";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Get, false));
    type Input = ReturnInput;
    type Output = ();
    type Error = ();
    type Progress = ();
}
/// Trusted logout continuations are supplied separately from authorization URLs.
/// Provider origins are already validated by acquisition/controller assembly.
pub fn definitions<C: Crypto>(
    providers: Vec<Provider>,
    returns: alloc::collections::BTreeMap<String, String>,
    crypto: impl Fn() -> C + Send + 'static,
) -> Result<Vec<Definition>, Error> {
    super::verification::validate_providers(&providers)?;
    for uri in returns.values() {
        let uri = url::Url::parse(uri).map_err(|_| Error::Invalid)?;
        if !matches!(uri.scheme(), "http" | "https")
            || uri.host_str().is_none()
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(vec![
        Definition::typed::<Release>(
            false,
            vec![],
            super::data(),
            &["clock"],
            move |tx, input, context| {
                let p = provider(&providers, &input.provider)?;
                let redirect = returns
                    .get(&input.continuation)
                    .ok_or(Error::Invalid)?
                    .clone();
                let bearer = context.bearer.as_deref().ok_or(Error::NotFound)?;
                let now = now(context)?;
                let session = super::for_logout(tx, bearer, now)?;
                if session.issuer != p.issuer || !super::same_secret(&session.csrf, &input.csrf) {
                    return Err(Error::NotFound.into());
                }
                let mut crypto = crypto();
                let state = random(&mut crypto)?;
                let binding = random(&mut crypto)?;
                super::revoke(tx, bearer)?;
                super::clear_attempts(tx, None, now)?;
                super::start(
                    tx,
                    &state,
                    &Attempt {
                        target: None,
                        binding: super::digest(&binding),
                        nonce: String::new(),
                        verifier: String::new(),
                        redirect: redirect.clone(),
                        issuer: p.issuer.clone(),
                        old_session: None,
                        logout: true,
                        expires: now.checked_add(300).ok_or(Error::Invalid)?,
                        processing: false,
                    },
                )?;
                context
                    .bearer_changed(Change::Clear)
                    .map_err(|_| Error::Invalid)?;
                Ok(Continuation {
                    endpoint: alloc::format!("{}/oauth/logout", p.issuer),
                    client: p.client.clone(),
                    redirect,
                    state,
                    binding,
                })
            },
        ),
        Definition::typed::<Returned>(
            false,
            vec![],
            super::data(),
            &["clock"],
            |tx, input, context| {
                super::consume(tx, &input.state, &input.binding, true, now(context)?)?;
                super::finish_logout(tx, &input.state)?;
                Ok(())
            },
        ),
    ])
}
