//! Portable OAuth acquisition. Operations persist private exchange requests;
//! controllers claim them before IO and store signature-verified results. The
//! originating operation completes by committing Identity's credential/session
//! rules. HTTP cookies and redirect encoding belong to the carrier adapter.
use super::{Attempt, Grant, Tokens, Validation};
use crate::{Crypto, Identity, Principal, attempt};
use alloc::{collections::BTreeMap, string::String, vec};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snap_store::{Error, Row, Transaction};
use snap_transport::{
    Operation,
    bearer::{Change, Receiver, Token},
    operation::{Context, Definition, TypedFailure},
};

/// Trusted provider and registered continuation configuration. Callers select
/// names only. Definitions and the verification controller validate URLs at assembly.
#[derive(Clone)]
pub struct Provider {
    pub name: String,
    pub issuer: String,
    pub client: String,
    pub authorization_endpoint: String,
    pub continuations: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginInput {
    pub provider: String,
    pub continuation: String,
}

/// Authorization instructions, not an HTTP response. Adapters encode the OAuth
/// parameters at the configured endpoint and keep the binding separate from the
/// authorization URL. Possession of the binding confers no session authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    pub endpoint: String,
    pub client: String,
    pub redirect: String,
    pub state: String,
    pub nonce: String,
    pub code_challenge: String,
    pub binding: String,
    pub expires: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub enum ProviderResponse {
    Code { code: String },
    Rejected { error: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallbackInput {
    pub provider: String,
    pub state: String,
    pub binding: String,
    pub issuer: String,
    pub response: ProviderResponse,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "code", deny_unknown_fields)]
pub enum AcquisitionError {
    Declined,
    ExchangeFailed,
}

pub struct Begin;
impl Operation for Begin {
    const NAME: &'static str = "identity.oauth-acquire";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Get, true));
    type Input = BeginInput;
    type Output = Authorization;
    type Error = AcquisitionError;
    type Progress = ();
}
pub struct Callback;
pub struct Link;
impl Operation for Link {
    const NAME: &'static str = "identity.oauth-link";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Post, true));
    type Input = BeginInput;
    type Output = Authorization;
    type Error = AcquisitionError;
    type Progress = ();
}
impl Operation for Callback {
    const NAME: &'static str = "identity.oauth-callback";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Get, false));
    type Input = CallbackInput;
    type Output = Principal;
    type Error = AcquisitionError;
    type Progress = ();
}

#[derive(Serialize, Deserialize)]
struct Continuation {
    state: String,
    provider: String,
}
#[derive(Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    attempt: Attempt,
    exchange: ExchangeState,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ExchangeState {
    Ready {
        provider: String,
    },
    Queued {
        provider: String,
        code: String,
    },
    Running {
        provider: String,
    },
    Verified {
        provider: String,
        tokens: Tokens,
        subject: String,
        profile: Value,
        now: i64,
    },
    Failed,
    Rejected,
}

/// A single claimed exchange, never serialized or exposed as an operation
/// result. Commit `claim` before using it for IO. Failure, disconnect or restart
/// does not release the fence and must not cause the code to be replayed.
pub(super) struct Exchange {
    key: String,
    provider: String,
    code: String,
    attempt: Attempt,
}
impl Exchange {
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn attempt(&self) -> &Attempt {
        &self.attempt
    }
}

pub(super) fn now(context: &Context) -> Result<i64, Error> {
    context
        .inputs
        .get("clock")
        .and_then(Value::as_i64)
        .filter(|now| *now >= 0)
        .ok_or(Error::Unavailable)
}
pub(super) fn random(crypto: &mut impl Crypto) -> Result<String, Error> {
    Ok(URL_SAFE_NO_PAD.encode(crypto.random()?))
}
pub(super) fn provider<'a>(providers: &'a [Provider], name: &str) -> Result<&'a Provider, Error> {
    providers
        .iter()
        .find(|p| p.name == name)
        .ok_or(Error::Invalid)
}

/// Hosts register these declarations through the ordinary operation registry.
/// They remain connectionless until shared ingress policy is expanded. No route
/// inventory or platform executor is installed by Identity.
pub fn definitions<C: Crypto>(
    identity: Identity,
    providers: alloc::vec::Vec<Provider>,
    crypto: impl Fn() -> C + Clone + Send + 'static,
) -> Result<crate::operation::Operations, Error> {
    super::verification::validate_providers(&providers)?;
    Ok(crate::operation::Operations {
        requests: vec![],
        preconnection: vec![
            begin::<Begin, C>(providers.clone(), crypto.clone(), false),
            begin::<Link, C>(providers.clone(), crypto.clone(), true),
            Definition::staged::<Callback, Continuation>(
                false,
                vec![],
                super::data(),
                &["clock"],
                move |tx, input, context| {
                    let p = provider(&providers, &input.provider)?;
                    if input.issuer != p.issuer
                        || input.state.len() > 256
                        || input.binding.len() > 256
                    {
                        return Err(Error::Invalid.into());
                    }
                    let exchange = match input.response {
                        ProviderResponse::Code { code }
                            if !code.is_empty() && code.len() < 8192 =>
                        {
                            ExchangeState::Queued {
                                provider: p.name.clone(),
                                code,
                            }
                        }
                        ProviderResponse::Rejected { error }
                            if !error.is_empty() && error.len() < 1024 =>
                        {
                            ExchangeState::Rejected
                        }
                        _ => return Err(Error::Invalid.into()),
                    };
                    let record: Record =
                        attempt::read(tx, super::ATTEMPTS, &super::attempt_id(&input.state))?
                            .ok_or(Error::NotFound)?;
                    if !matches!(record.exchange, ExchangeState::Ready { provider } if provider == p.name)
                    {
                        return Err(Error::NotFound.into());
                    }
                    let pending =
                        super::consume(tx, &input.state, &input.binding, false, now(context)?)?;
                    if pending.issuer != p.issuer
                        || !p.continuations.values().any(|uri| uri == &pending.redirect)
                    {
                        return Err(Error::Invalid.into());
                    }
                    attempt::write(
                        tx,
                        super::ATTEMPTS,
                        &super::attempt_id(&input.state),
                        &Record {
                            attempt: pending,
                            exchange,
                        },
                        false,
                    )?;
                    Ok(Continuation {
                        state: input.state,
                        provider: p.name.clone(),
                    })
                },
                move |tx, request, context| {
                    let key = super::attempt_id(&request.state);
                    let record: Record =
                        attempt::read(tx, super::ATTEMPTS, &key)?.ok_or(Error::NotFound)?;
                    let (tokens, subject, profile, now) = match record.exchange {
                        ExchangeState::Verified {
                            provider,
                            tokens,
                            subject,
                            profile,
                            now,
                        } if provider == request.provider => (tokens, subject, profile, now),
                        ExchangeState::Rejected => {
                            return Err(TypedFailure::Application(AcquisitionError::Declined));
                        }
                        ExchangeState::Failed => {
                            return Err(TypedFailure::Application(
                                AcquisitionError::ExchangeFailed,
                            ));
                        }
                        _ => return Err(Error::Unavailable.into()),
                    };
                    let mut crypto = crypto();
                    let bearer = random(&mut crypto)?;
                    let grant = Grant {
                        id: super::digest(&bearer),
                        owner: super::owner(&record.attempt.issuer, &subject),
                        subject,
                        issuer: record.attempt.issuer,
                        nonce: record.attempt.nonce,
                        profile,
                        tokens,
                        csrf: random(&mut crypto)?,
                        expires: now
                            .checked_add(identity.lifetime_seconds)
                            .ok_or(Error::Invalid)?,
                        refreshing: false,
                        version: 1,
                    };
                    super::issue(tx, &request.state, &grant, now)?;
                    let principal = identity.resolve(tx, &crypto, &bearer, now)?;
                    context
                        .bearer_changed(Change::Set(Token::new(bearer)))
                        .map_err(|_| Error::Invalid)?;
                    Ok(principal)
                },
            ),
        ],
    })
}

fn begin<O, C: Crypto>(
    providers: alloc::vec::Vec<Provider>,
    crypto: impl Fn() -> C + Send + 'static,
    link: bool,
) -> Definition
where
    O: Operation<
            Input = BeginInput,
            Output = Authorization,
            Error = AcquisitionError,
            Progress = (),
        >,
{
    Definition::typed::<O>(
        link,
        vec![],
        super::data(),
        &["clock"],
        move |tx, input, context| {
            let p = provider(&providers, &input.provider)?;
            let redirect = p
                .continuations
                .get(&input.continuation)
                .ok_or(Error::Invalid)?
                .clone();
            let now = now(context)?;
            let mut crypto = crypto();
            let state = random(&mut crypto)?;
            let binding = random(&mut crypto)?;
            let mut pending = Attempt {
                target: None,
                binding: super::digest(&binding),
                nonce: random(&mut crypto)?,
                verifier: random(&mut crypto)?,
                redirect: redirect.clone(),
                issuer: p.issuer.clone(),
                old_session: context.bearer.as_deref().map(super::digest),
                logout: false,
                expires: now.checked_add(300).ok_or(Error::Invalid)?,
                processing: false,
            };
            super::clear_attempts(tx, None, now)?;
            super::start(tx, &state, &pending)?;
            if link {
                let bearer = context.bearer.as_deref().ok_or(Error::NotFound)?;
                let principal = Identity::default().fresh(tx, &crypto, bearer, now)?;
                pending.target = Some(principal.identity);
            }
            attempt::write(
                tx,
                super::ATTEMPTS,
                &super::attempt_id(&state),
                &Record {
                    attempt: pending.clone(),
                    exchange: ExchangeState::Ready {
                        provider: p.name.clone(),
                    },
                },
                false,
            )?;
            Ok(Authorization {
                endpoint: p.authorization_endpoint.clone(),
                client: p.client.clone(),
                redirect,
                state,
                nonce: pending.nonce,
                code_challenge: super::digest(&pending.verifier),
                binding,
                expires: pending.expires,
            })
        },
    )
}

/// Controller selection from committed private attempt rows. Recovery must run
/// `oauth::recover` before controller scanning; interrupted exchanges are discarded.
pub(super) fn pending(row: &Row) -> bool {
    row.get("data")
        .and_then(|value| match value {
            snap_store::Value::Text(value) => serde_json::from_str::<Record>(value).ok(),
            _ => None,
        })
        .is_some_and(|record| matches!(record.exchange, ExchangeState::Queued { .. }))
}

pub(super) fn claim(tx: &mut Transaction<'_>, key: &str, now: i64) -> Result<Exchange, Error> {
    let mut record: Record = attempt::read(tx, super::ATTEMPTS, key)?.ok_or(Error::NotFound)?;
    if !record.attempt.processing || record.attempt.expires <= now {
        return Err(Error::NotFound);
    }
    let ExchangeState::Queued { provider, code } = record.exchange else {
        return Err(Error::NotFound);
    };
    record.exchange = ExchangeState::Running {
        provider: provider.clone(),
    };
    attempt::write(tx, super::ATTEMPTS, key, &record, false)?;
    Ok(Exchange {
        key: key.into(),
        provider,
        code,
        attempt: record.attempt,
    })
}

/// Constructed only by signature and claim verification, never by deserialization.
pub(super) struct Verified {
    pub tokens: Tokens,
    subject: String,
    expires: i64,
    checked_at: i64,
}

pub(super) fn verify(
    crypto: &impl Crypto,
    exchange: &Exchange,
    provider: &Provider,
    response: &Value,
    jwks: &Value,
    now: i64,
) -> Result<Verified, Error> {
    if now < 0
        || provider.name != exchange.provider
        || provider.issuer != exchange.attempt.issuer
        || exchange.attempt.expires <= now
    {
        return Err(Error::NotFound);
    }
    let token = response["id_token"].as_str().ok_or(Error::Invalid)?;
    let claims = crypto.verify_token(token, jwks)?;
    let tokens = super::validate_tokens(
        response,
        &claims,
        Validation {
            issuer: &provider.issuer,
            client: &provider.client,
            nonce: Some(&exchange.attempt.nonce),
            previous: None,
            now,
        },
    )?;
    Ok(Verified {
        tokens,
        subject: claims["sub"].as_str().ok_or(Error::Invalid)?.into(),
        expires: claims["exp"].as_i64().ok_or(Error::Invalid)?,
        checked_at: now,
    })
}

/// Recheck the consumed attempt and proof expiry after all IO. Profile retrieval
/// does not extend token validity. No session is issued until operation completion.
pub(super) fn record_verified(
    tx: &mut Transaction<'_>,
    exchange: &Exchange,
    verified: Verified,
    profile: Value,
    now: i64,
) -> Result<(), Error> {
    let mut record: Record =
        attempt::read(tx, super::ATTEMPTS, &exchange.key)?.ok_or(Error::NotFound)?;
    if !matches!(&record.exchange, ExchangeState::Running { provider } if provider == &exchange.provider)
        || !record.attempt.processing
        || record.attempt.expires <= now
        || now < verified.checked_at
        || verified.expires <= now
        || verified.tokens.access_expires <= now
    {
        return Err(Error::NotFound);
    }
    if profile["sub"].as_str() != Some(&verified.subject) {
        return Err(Error::Invalid);
    }
    record.exchange = ExchangeState::Verified {
        provider: exchange.provider.clone(),
        tokens: verified.tokens,
        subject: verified.subject,
        profile,
        now,
    };
    attempt::write(tx, super::ATTEMPTS, &exchange.key, &record, false)
}

/// Record a failed/uncertain exchange without making its code claimable again.
pub(super) fn failed(tx: &mut Transaction<'_>, exchange: &Exchange) -> Result<(), Error> {
    let mut record: Record =
        attempt::read(tx, super::ATTEMPTS, &exchange.key)?.ok_or(Error::NotFound)?;
    if !matches!(&record.exchange, ExchangeState::Running { provider } if provider == &exchange.provider)
    {
        return Err(Error::NotFound);
    }
    record.exchange = ExchangeState::Failed;
    attempt::write(tx, super::ATTEMPTS, &exchange.key, &record, false)
}
