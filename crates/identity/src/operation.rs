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
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialInput {
    pub locator: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameInput {
    pub locator: String,
    pub label: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationInput {
    pub label: String,
    pub binding: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationInput {
    pub locator: Option<String>,
    pub name: Option<String>,
    pub binding: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasskeyProof {
    pub attempt: String,
    pub binding: String,
    pub response: Value,
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
    RemoveCredential,
    "identity.credential-remove",
    CredentialInput,
    ()
);
contract!(
    RenameCredential,
    "identity.credential-rename",
    RenameInput,
    ()
);
contract!(LinkPassword, "identity.password-link", Proof, ());
contract!(
    BeginRegistration,
    "identity.passkey-register",
    RegistrationInput,
    crate::passkey::Challenge
);
contract!(
    FinishRegistration,
    "identity.passkey-registered",
    PasskeyProof,
    Principal
);
contract!(
    BeginAuthentication,
    "identity.passkey-authenticate",
    AuthenticationInput,
    crate::passkey::Challenge
);
contract!(
    FinishAuthentication,
    "identity.passkey-authenticated",
    PasskeyProof,
    Principal
);
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
/// App-owned metadata lookup for named, nonresident authenticators. Candidate
/// principals may be selected by a name, but proof alone determines authority;
/// names and matching email metadata never link accounts.
pub struct PasskeyLookup {
    pub data: Data,
    pub lookup: Box<dyn Fn(&mut Transaction<'_>, &str) -> Result<Vec<String>, Error> + Send>,
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
    let remove_crypto = crypto.clone();
    let rename_crypto = crypto.clone();
    let link_crypto = crypto.clone();
    let enrollment_data = enrollment
        .as_ref()
        .map(|hook| hook.data.clone())
        .unwrap_or_default();
    Operations {
        requests: Vec::new(),
        preconnection: vec![
            Definition::typed::<RemoveCredential>(
                true,
                vec![],
                identity.data(),
                &["clock"],
                move |tx, input, context| {
                    identity.remove_credential(
                        tx,
                        &remove_crypto(),
                        context.bearer.as_deref().ok_or(Error::NotFound)?,
                        &input.locator,
                        now(context)?,
                    )?;
                    Ok(())
                },
            ),
            Definition::typed::<RenameCredential>(
                true,
                vec![],
                identity.data(),
                &["clock"],
                move |tx, input, context| {
                    identity.rename_credential(
                        tx,
                        &rename_crypto(),
                        context.bearer.as_deref().ok_or(Error::NotFound)?,
                        &input.locator,
                        &input.label,
                        now(context)?,
                    )?;
                    Ok(())
                },
            ),
            Definition::typed::<LinkPassword>(
                true,
                vec![],
                identity.data(),
                &["clock"],
                move |tx, input, context| {
                    identity.link_password(
                        tx,
                        &mut link_crypto(),
                        context.bearer.as_deref().ok_or(Error::NotFound)?,
                        &input.email,
                        &input.password,
                        now(context)?,
                    )?;
                    Ok(())
                },
            ),
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
            | RemoveCredential::NAME
            | RenameCredential::NAME
            | LinkPassword::NAME
            | BeginRegistration::NAME
            | FinishRegistration::NAME
            | BeginAuthentication::NAME
            | FinishAuthentication::NAME
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
        (RemoveCredential::NAME, Post, true),
        (RenameCredential::NAME, Post, true),
        (LinkPassword::NAME, Post, true),
    ]
    .into_iter()
    .map(|(operation, method, read_bearer)| HttpRoute {
        operation,
        method,
        read_bearer,
    })
    .collect()
}

/// Hosts select passkey operations independently of password and OAuth flows.
/// Without an enrollment hook, registration requires a current session. With a
/// hook, a new identity is initialized in the same transaction using its label.
pub fn passkey_definitions<C: Crypto, W: crate::passkey::WebAuthn + Clone + Send + 'static>(
    identity: Identity,
    crypto: impl Fn() -> C + Clone + Send + 'static,
    webauthn: W,
    enrollment: Option<Enrollment>,
    lookup: Option<PasskeyLookup>,
) -> Vec<Definition> {
    let begin_crypto = crypto.clone();
    let register_crypto = crypto.clone();
    let auth_crypto = crypto.clone();
    let begin_web = webauthn.clone();
    let register_web = webauthn.clone();
    let auth_web = webauthn.clone();
    let can_enroll = enrollment.is_some();
    let data = enrollment
        .as_ref()
        .map(|h| h.data.clone())
        .unwrap_or_default();
    let lookup_data = lookup.as_ref().map(|h| h.data.clone()).unwrap_or_default();
    vec![
        Definition::typed::<BeginRegistration>(
            false,
            vec![],
            crate::passkey::Passkeys::data(),
            &["clock"],
            move |tx, input, context| {
                if !can_enroll && context.principal.is_none() {
                    return Err(Error::NotFound.into());
                }
                Ok(crate::passkey::Passkeys::new(identity).begin_registration(
                    tx,
                    &mut begin_crypto(),
                    &begin_web,
                    context.bearer.as_deref(),
                    &input.binding,
                    &input.label,
                    now(context)?,
                )?)
            },
        ),
        Definition::typed::<FinishRegistration>(
            false,
            vec![],
            crate::passkey::Passkeys::data().and(data),
            &["clock"],
            move |tx, proof, context| {
                if !can_enroll && context.principal.is_none() {
                    return Err(Error::NotFound.into());
                }
                let issued = crate::passkey::Passkeys::new(identity).finish_registration(
                    tx,
                    &mut register_crypto(),
                    &register_web,
                    &proof.attempt,
                    &proof.binding,
                    context.bearer.as_deref(),
                    proof.response.clone(),
                    now(context)?,
                )?;
                if context.bearer.is_none() {
                    if let Some(hook) = &enrollment {
                        let labels = Credential::summaries(tx, &issued.principal.identity)?;
                        (hook.initialize)(
                            tx,
                            &issued.principal,
                            &labels.first().ok_or(Error::Invalid)?.label,
                        )?;
                    }
                }
                context
                    .bearer_changed(Change::Set(Token::new(issued.bearer)))
                    .map_err(|_| Error::Invalid)?;
                Ok(issued.principal)
            },
        ),
        Definition::typed::<BeginAuthentication>(
            false,
            vec![anonymous()],
            crate::passkey::Passkeys::data().and(lookup_data),
            &["clock"],
            move |tx, input, context| {
                if let Some(name) = &input.name {
                    if input.locator.is_some() {
                        return Err(Error::Invalid.into());
                    }
                    let hook = lookup.as_ref().ok_or(Error::NotFound)?;
                    let identities = (hook.lookup)(tx, name)?;
                    return Ok(
                        crate::passkey::Passkeys::new(identity).begin_authentication_for(
                            tx,
                            &mut auth_crypto(),
                            &auth_web,
                            Some(&identities),
                            &input.binding,
                            now(context)?,
                        )?,
                    );
                }
                Ok(
                    crate::passkey::Passkeys::new(identity).begin_authentication(
                        tx,
                        &mut auth_crypto(),
                        &auth_web,
                        input.locator.as_deref(),
                        &input.binding,
                        now(context)?,
                    )?,
                )
            },
        ),
        Definition::typed::<FinishAuthentication>(
            false,
            vec![anonymous()],
            crate::passkey::Passkeys::data(),
            &["clock"],
            move |tx, proof, context| {
                let issued = crate::passkey::Passkeys::new(identity).finish_authentication(
                    tx,
                    &mut crypto(),
                    &webauthn,
                    &proof.attempt,
                    &proof.binding,
                    proof.response.clone(),
                    now(context)?,
                )?;
                context
                    .bearer_changed(Change::Set(Token::new(issued.bearer)))
                    .map_err(|_| Error::Invalid)?;
                Ok(issued.principal)
            },
        ),
    ]
}
pub fn passkey_http_routes() -> Vec<snap_transport::carrier::HttpRoute> {
    use snap_transport::carrier::{HttpMethod::Post, HttpRoute};
    [
        (BeginRegistration::NAME, true),
        (FinishRegistration::NAME, true),
        (BeginAuthentication::NAME, false),
        (FinishAuthentication::NAME, false),
    ]
    .into_iter()
    .map(|(operation, read_bearer)| HttpRoute {
        operation,
        method: Post,
        read_bearer,
    })
    .collect()
}
