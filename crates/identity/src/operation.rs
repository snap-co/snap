//! Identity-owned contracts and operations over Credential and private Sessions.
use crate::{
    Credential, Crypto, Identity, Principal, ReleaseScope, SessionSummary, session::Sessions,
};
use alloc::{boxed::Box, string::String, vec, vec::Vec};
use snap_store::{Data, Error, Transaction};
use snap_transport::{
    Operation, Value,
    bearer::{Change, Receiver, Token},
    operation::{Context, Definition, Guard, TypedFailure},
};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub email: String,
    pub password: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseInput {
    pub scope: ReleaseScope,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", deny_unknown_fields)]
pub enum IdentityError {
    Conflict,
}
macro_rules! contract {
    ($type:ident, $name:literal, $input:ty, $output:ty) => {
        pub struct $type;
        impl Operation for $type {
            const NAME: &'static str = $name;
            type Input = $input;
            type Output = $output;
            type Error = IdentityError;
            type Progress = ();
        }
    };
}
contract!(Enroll, "identity.enroll", Proof, Principal);
contract!(Acquire, "identity.acquire", Proof, Principal);
contract!(Fetch, "identity.fetch", (), Option<Principal>);
contract!(Release, "identity.release", ReleaseInput, ());
contract!(ListSessions, "identity.sessions", (), Vec<SessionSummary>);
contract!(
    ListCredentials,
    "identity.credentials",
    (),
    Vec<crate::CredentialSummary>
);

pub type Initialize =
    Box<dyn Fn(&mut Transaction<'_>, &Principal, &str) -> Result<(), Error> + Send>;
/// Composition can initialize its own data when a new identity is enrolled.
/// It cannot replace Identity's contracts, credential policy or response shape.
pub struct Enrollment {
    pub data: Data,
    pub initialize: Initialize,
}
#[derive(Default)]
pub struct Operations {
    pub requests: Vec<Definition>,
    pub preconnection: Vec<Definition>,
}
fn now(context: &Context) -> Result<i64, Error> {
    context
        .inputs
        .get("clock")
        .and_then(Value::as_i64)
        .ok_or(Error::Unavailable)
}
fn anonymous() -> Guard {
    Guard::new(|_, _, context| {
        if context.bearer.is_some() {
            return Err(snap_transport::Error::InvalidInput.into());
        }
        Ok(())
    })
}
fn principal(context: &Context) -> Result<&Principal, Error> {
    context.principal.as_ref().ok_or(Error::NotFound)
}

pub fn definitions<C: Crypto>(
    identity: Identity,
    crypto: impl Fn() -> C + Clone + Send + 'static,
    enrollment: Option<Enrollment>,
) -> Operations {
    let enroll_crypto = crypto.clone();
    let acquire_crypto = crypto.clone();
    let release_crypto = crypto.clone();
    let sessions_crypto = crypto.clone();
    let enrollment_data = enrollment
        .as_ref()
        .map(|hook| hook.data.clone())
        .unwrap_or_default();
    Operations {
        requests: Vec::new(),
        preconnection: vec![
            Definition::typed::<Enroll>(
                false,
                vec![anonymous()],
                identity.data().and(enrollment_data),
                &["clock"],
                move |tx, proof, context| {
                    let issued = match identity.enroll(
                        tx,
                        &mut enroll_crypto(),
                        &proof.email,
                        &proof.password,
                        now(context)?,
                    ) {
                        Err(Error::Constraint) => {
                            return Err(TypedFailure::Application(IdentityError::Conflict));
                        }
                        result => result?,
                    };
                    if let Some(hook) = &enrollment {
                        (hook.initialize)(tx, &issued.principal, &crate::email_key(&proof.email)?)?;
                    }
                    context
                        .bearer_changed(Change::Set(Token::new(issued.bearer)))
                        .map_err(|_| Error::Invalid)?;
                    Ok(issued.principal)
                },
            ),
            Definition::typed::<Acquire>(
                false,
                vec![anonymous()],
                identity.data(),
                &["clock"],
                move |tx, proof, context| {
                    let issued = identity.acquire(
                        tx,
                        &mut acquire_crypto(),
                        &proof.email,
                        &proof.password,
                        now(context)?,
                    )?;
                    context
                        .bearer_changed(Change::Set(Token::new(issued.bearer)))
                        .map_err(|_| Error::Invalid)?;
                    Ok(issued.principal)
                },
            ),
            Definition::typed::<Fetch>(false, vec![], identity.data(), &[], |_, _, context| {
                if context.principal.is_none() && context.bearer.is_some() {
                    context
                        .bearer_changed(Change::Clear)
                        .map_err(|_| Error::Invalid)?;
                }
                Ok(context.principal.clone())
            }),
            Definition::typed::<Release>(
                true,
                vec![],
                identity.data(),
                &[],
                move |tx, input, context| {
                    let actor = principal(context)?.identity.clone();
                    let bearer = context.bearer.as_deref().ok_or(Error::NotFound)?;
                    identity.release_accepted(
                        tx,
                        &release_crypto(),
                        &actor,
                        bearer,
                        input.scope,
                    )?;
                    if input.scope != ReleaseScope::Others {
                        context
                            .bearer_changed(Change::Clear)
                            .map_err(|_| Error::Invalid)?;
                    }
                    Ok(())
                },
            ),
            Definition::typed::<ListSessions>(
                true,
                vec![],
                identity.data(),
                &["clock"],
                move |tx, _, context| {
                    let crypto = sessions_crypto();
                    let current = crypto.digest(context.bearer.as_deref().ok_or(Error::NotFound)?);
                    Ok(Sessions::summaries(
                        tx,
                        &crypto,
                        &principal(context)?.identity,
                        &current,
                        now(context)?,
                    )?)
                },
            ),
            Definition::typed::<ListCredentials>(
                true,
                vec![],
                Credential::data(),
                &[],
                |tx, _, context| Ok(Credential::summaries(tx, &principal(context)?.identity)?),
            ),
        ],
    }
}
pub fn recognizes(name: &str) -> bool {
    matches!(
        name,
        Enroll::NAME
            | Acquire::NAME
            | Fetch::NAME
            | Release::NAME
            | ListSessions::NAME
            | ListCredentials::NAME
    )
}

/// Identity owns the HTTP mapping as well as the native operation contracts.
pub fn http_routes() -> Vec<snap_transport::carrier::HttpRoute> {
    use snap_transport::carrier::{
        HttpMethod::{Get, Post},
        HttpRoute,
    };
    [
        (Enroll::NAME, Post, false),
        (Acquire::NAME, Post, false),
        (Fetch::NAME, Get, true),
        (Release::NAME, Post, true),
        (ListSessions::NAME, Get, true),
        (ListCredentials::NAME, Get, true),
    ]
    .into_iter()
    .map(|(operation, method, read_bearer)| HttpRoute {
        operation,
        method,
        read_bearer,
    })
    .collect()
}
