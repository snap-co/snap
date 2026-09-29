//! Portable OAuth 2.0 / OpenID Connect authorization-code issuer.
//!
//! Synchronous, `no_std` with `alloc`, and IO-free. All reads and writes go
//! through the caller's `&mut snap_store::Transaction`; hosts own randomness,
//! SHA-256 digests, RS256 signing, clocks, session authority, and HTTP mapping.
//!
//! Raw handles, codes, and bearer tokens never enter Store: only their digests
//! are retained. Every function returns `Result<Outcome, snap_store::Error>`:
//! the `Err` channel carries Store failures (including terminal residency
//! `Miss`es, which must never be mapped to protocol errors), while protocol
//! results -- including `invalid_grant` replays that still revoke a grant
//! family -- are `Ok` variants whose writes commit through the host's
//! `Store::run`. Hosts must release handles, codes, and tokens only from the
//! `Committed` value; a rejected or fenced commit publishes nothing.
//!
//! Host duties per call, in the same Store transaction where noted:
//! - Load [`TABLES`] (and the Identity tables backing [`Authority`]) before
//!   calling; a `Miss` means the host must load and the caller must retry.
//! - Resolve the browser bearer to a [`BrowserSession`] (subject plus the
//!   Identity session *digest*, never the raw bearer) and derive `auth_time`
//!   as `expires - 30 days`. The-sync check runs again inside each call
//!   through [`Authority`], so verification cannot go stale between resolve
//!   and commit.
//! - Prepare fresh profile [`Claims`] for the grant subject in the same
//!   transaction before code exchange, refresh, and userinfo, so ID tokens and
//!   userinfo reflect the current profile. Use [`token_subject`] to resolve
//!   the subject from the presented code/token without touching private row
//!   schema; the endpoint call afterward still governs success.
//! - Verify `id_token_hint` signatures with pinned RS256 before constructing
//!   [`IdHint`]: check `iss` against the issuer, require `sub`/`sid`, and
//!   accept expired hints (they are logout hints, never authentication).
//! - Reject `client_secret` in form bodies and duplicate `Authorization`
//!   headers before building [`ClientAuth`] (pass `body_had_secret` through).
//! - Map outcomes to HTTP: redirects, 400/401/403 bodies, `no-store`,
//!   `frame-ancestors 'none'`, and `WWW-Authenticate`. Content-type gating
//!   (415), HTML rendering, and cookie clearing stay host-owned.
//!
//! Time limits use host-supplied Unix seconds, matching Identity: codes 60s,
//! login/consent/logout continuations 5min, access/ID tokens 10min, refresh
//! families 30 days absolute. Grant families additionally depend on the
//! originating Identity session: expiry or revocation there invalidates every
//! exchange, refresh, and userinfo lookup without extra writes.
//!
//! Unsupported on purpose (return the documented outcome instead of branching
//! into new flows): request objects, dynamic registration, implicit/hybrid
//! flows, encrypted ID tokens, `offline_access`, ACR assertions, token
//! introspection, and front/back-channel logout. Display, locale, and ACR hint
//! parameters are accepted and ignored by the host; they need no portable
//! input.
#![no_std]
extern crate alloc;
pub mod relying_party;

use alloc::{string::String, vec::Vec};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use snap_store::{Error, Row, Transaction, Value};

/// Ordered migration declarations for the issuer tables.
pub const MIGRATION: &str = include_str!("../migrations/0001_oidc.toml");
/// Store tables owned by this module. Hosts load these to arrange residency.
pub const TABLES: [&str; 3] = ["oidc.flows", "oidc.grants", "oidc.tokens"];
const FLOWS: &str = TABLES[0];
const GRANTS: &str = TABLES[1];
const TOKENS: &str = TABLES[2];

/// Authorization codes live 60 seconds.
pub const CODE_SECONDS: i64 = 60;
/// Login, consent, and logout continuations live five minutes.
pub const CONTINUATION_SECONDS: i64 = 300;
/// Access tokens and ID tokens live ten minutes.
pub const ACCESS_SECONDS: i64 = 600;
/// Refresh families live 30 days absolute and depend on the login session.
pub const GRANT_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Host randomness, hashing, and signing. Implementations must use
/// cryptographically secure randomness, real SHA-256 for [`Host::digest`]
/// (at least 16 bytes out), and pinned RS256 for [`Host::sign`]; test
/// adapters may be deterministic. Failures abort the transaction.
pub trait Host {
    /// Fresh base64url material with at least 256 unpredictable bits.
    fn random(&mut self) -> Result<String, Error>;
    /// Raw SHA-256 digest of a handle, code, or bearer token.
    fn digest(&self, secret: &str) -> Vec<u8>;
    /// RS256-sign the prepared ID-token claims.
    fn sign(&self, claims: &serde_json::Value) -> Result<String, Error>;
}

/// Host session authority. The coordinator's Authy host implements this with
/// `Identity::resolve_digest`, so every exchange, refresh, and userinfo
/// validates the stored session digest transactionally. A digest is not a
/// wire credential: hosts must never accept a caller-supplied digest in place
/// of a bearer.
pub trait Authority {
    /// Resolve a stored session digest to its identity subject, or
    /// `NotFound` when revoked/expired. `Miss` stays terminal.
    fn subject(&self, tx: &mut Transaction<'_>, session: &[u8], now: i64) -> Result<String, Error>;
}

/// Statically registered relying party. Redirects match exactly; there are no
/// wildcards. `secret_digest` is `None` for public PKCE clients and
/// `Some(SHA-256(secret))` for confidential clients, which must authenticate
/// with `client_secret_basic`. Only the digest is retained here.
///
/// Deliberately has no `Debug` implementation so secret digests never enter
/// diagnostics.
pub struct Client {
    pub id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub post_logout_redirect_uris: Vec<String>,
    pub secret_digest: Option<Vec<u8>>,
}

impl Client {
    /// Public PKCE-only registration.
    pub fn public(
        id: &str,
        name: &str,
        redirect_uri: &str,
        post_logout_redirect_uri: &str,
    ) -> Result<Self, Error> {
        if id.is_empty() || name.is_empty() || redirect_uri.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            id: id.into(),
            name: name.into(),
            redirect_uris: alloc::vec![redirect_uri.into()],
            post_logout_redirect_uris: alloc::vec![post_logout_redirect_uri.into()],
            secret_digest: None,
        })
    }

    /// Confidential registration holding only the secret digest.
    pub fn confidential(
        id: &str,
        name: &str,
        redirect_uri: &str,
        post_logout_redirect_uri: &str,
        secret_digest: Vec<u8>,
    ) -> Result<Self, Error> {
        if id.is_empty() || name.is_empty() || redirect_uri.is_empty() || secret_digest.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            id: id.into(),
            name: name.into(),
            redirect_uris: alloc::vec![redirect_uri.into()],
            post_logout_redirect_uris: alloc::vec![post_logout_redirect_uri.into()],
            secret_digest: Some(secret_digest),
        })
    }
}

/// Issuer configuration: canonical origin plus the static client registry.
///
/// Has no `Debug` implementation so client secret digests never enter
/// diagnostics.
pub struct Config {
    pub issuer: String,
    pub clients: Vec<Client>,
}

impl Config {
    /// Static registry lookup. Unknown ids are protocol errors, never misses.
    pub fn client(&self, id: &str) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }
}

/// Fresh profile claims prepared by the host in the same transaction for the
/// grant subject. Email comes from Identity; `email_verified` stays false
/// until a real verification flow exists. `updated_at` is a Unix-seconds
/// profile timestamp, or `<= 0` when the profile store keeps none: the claim
/// is then omitted rather than fabricated.
///
/// `Debug` is derived: these are fixture-safe profile fields, never raw
/// tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub updated_at: i64,
}

/// Validated browser session: subject plus the Identity session digest (never
/// the raw bearer) and its authentication time (`expires - 30 days`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserSession {
    pub subject: String,
    pub session: Vec<u8>,
    pub auth_time: i64,
}

/// Host-verified logout hint audience. The host verifies the RS256 signature,
/// checks `iss`, requires `sub`/`sid`, and accepts expired hints before
/// constructing this; only the audience is needed portably.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdHint {
    pub aud: String,
}

/// Client authentication material parsed by the host from the `Authorization`
/// header and form. The host must reject `client_secret` form fields and
/// duplicate headers first and surface that via `body_had_secret`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientAuth<'a> {
    pub client_id: &'a str,
    pub secret: Option<&'a str>,
    pub body_had_secret: bool,
}

/// Issued bearer material. Has no `Debug` implementation: release only from
/// `Store::run`'s `Committed` value, never from diagnostics or client views.
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub id_token: String,
    pub expires_in: i64,
    pub scope: String,
}

/// Standard discovery document. Pure function of configuration; no Store use.
pub fn discovery(config: &Config) -> serde_json::Value {
    let endpoint = |path: &str| alloc::format!("{}{path}", config.issuer);
    serde_json::json!({
        "issuer": config.issuer,
        "authorization_endpoint": endpoint("/oauth/authorize"),
        "token_endpoint": endpoint("/oauth/token"),
        "userinfo_endpoint": endpoint("/oauth/userinfo"),
        "jwks_uri": endpoint("/oauth/jwks"),
        "revocation_endpoint": endpoint("/oauth/revoke"),
        "end_session_endpoint": endpoint("/oauth/logout"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_basic"],
        "revocation_endpoint_auth_methods_supported": ["none", "client_secret_basic"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": ["openid", "profile", "email"],
        "authorization_response_iss_parameter_supported": true,
        "claims_supported": ["iss", "sub", "aud", "exp", "iat", "auth_time", "nonce", "sid", "at_hash", "name", "email", "email_verified", "updated_at"],
        "request_parameter_supported": false,
        "request_uri_parameter_supported": false,
        "claims_parameter_supported": false,
    })
}

/// Authorization request parameters parsed by the host from the query/form.
/// Display, locale, and ACR hints are accepted-and-ignored host-side and need
/// no field here.
pub struct AuthorizeRequest<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub response_type: &'a str,
    pub scope: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    pub code_challenge: &'a str,
    pub code_challenge_method: &'a str,
    pub prompt: &'a str,
    pub max_age: Option<&'a str>,
    /// Host-verified `id_token_hint` subject, if the hint was present.
    pub hint_subject: Option<&'a str>,
    pub response_mode: Option<&'a str>,
    pub has_request: bool,
    pub has_request_uri: bool,
    pub has_registration: bool,
}

/// `authorize` result. `ShowConsent`/`RequireLogin` carry raw handles the host
/// embeds only after commit. `RedirectError` targets the registered redirect;
/// `DirectError` must not redirect (unknown client or redirect).
pub enum AuthorizeOutcome {
    ShowConsent {
        handle: String,
    },
    RequireLogin {
        handle: String,
        reauth: bool,
    },
    RedirectError {
        redirect: String,
        error: &'static str,
        description: &'static str,
        state: String,
    },
    DirectError {
        description: &'static str,
    },
}

/// Login-continuation resume input: the raw handle from the `resume` query.
pub struct ResumeRequest<'a> {
    pub handle: &'a str,
}

/// `resume` result. `LoginRequired` means the browser must complete a fresh
/// sign-in with the requested account first.
pub enum ResumeOutcome {
    ShowConsent { handle: String },
    InvalidGrant,
    LoginRequired,
}

/// Consent decision input. `origin` is the request Origin header; mismatches
/// are forbidden before any Store read.
pub struct ConsentRequest<'a> {
    pub handle: &'a str,
    pub decision: &'a str,
    pub origin: &'a str,
}

/// `consent` result. `Redirect` carries the complete client redirect URI with
/// `code` (allow) or nothing extra (deny builds its own error redirect);
/// see constructors below.
pub enum ConsentOutcome {
    Redirect { uri: String },
    RedirectError { redirect: String, state: String },
    InvalidGrant,
    Forbidden,
}

/// Code-redemption input. `claims` are host-prepared fresh profile claims for
/// the grant subject, loaded in the same transaction.
pub struct ExchangeRequest<'a> {
    pub auth: &'a ClientAuth<'a>,
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub verifier: &'a str,
    pub claims: &'a Claims,
}

/// `exchange_code` result. Replay of a consumed code deactivates its grant
/// family and still commits; the caller observes `InvalidGrant`.
pub enum ExchangeOutcome {
    Issued(Tokens),
    InvalidGrant,
    InvalidClient,
}

/// Refresh input. A supplied `scope` must exactly equal the granted scope.
pub struct RefreshRequest<'a> {
    pub auth: &'a ClientAuth<'a>,
    pub token: &'a str,
    pub scope: Option<&'a str>,
    pub claims: &'a Claims,
}

/// `refresh` result. Reuse of a rotated token revokes the whole family and
/// still commits; the caller observes `InvalidGrant`.
pub enum RefreshOutcome {
    Issued(Tokens),
    InvalidGrant,
    InvalidClient,
    InvalidScope,
}

/// Userinfo input: raw access token plus host-prepared fresh claims.
pub struct UserinfoRequest<'a> {
    pub token: &'a str,
    pub claims: &'a Claims,
}

/// `userinfo` result. No writes are staged; validation is read-only.
pub enum UserinfoOutcome {
    Claims(serde_json::Value),
    InvalidToken,
}

/// Revocation input: any token of the family plus client authentication.
pub struct RevokeRequest<'a> {
    pub auth: &'a ClientAuth<'a>,
    pub token: &'a str,
}

/// `revoke` result. Unknown tokens and cross-client tokens still report
/// `Revoked` without writes, leaking nothing.
pub enum RevokeOutcome {
    Revoked,
    InvalidClient,
}

/// Logout-initiation input. `hint` is host-verified (see [`IdHint`]).
pub struct LogoutRequest<'a> {
    pub client_id: Option<&'a str>,
    pub post_logout_redirect_uri: Option<&'a str>,
    pub hint: Option<&'a IdHint>,
    pub state: &'a str,
    pub session: Option<&'a BrowserSession>,
}

/// `logout` result. `ShowConfirm` carries a raw handle for the host's
/// confirmation page; `Redirect` needs no confirmation (no session).
pub enum LogoutOutcome {
    ShowConfirm { handle: String },
    Redirect { uri: String },
    DirectError { description: &'static str },
}

/// Logout-confirmation input: raw handle plus the Origin header.
pub struct LogoutConfirm<'a> {
    pub handle: &'a str,
    pub origin: &'a str,
    pub session: Option<&'a BrowserSession>,
}

/// `logout_confirm` result. The host ends the Identity session in the same
/// transaction after `Redirect`, which transitively invalidates its grants.
pub enum LogoutConfirmOutcome {
    Redirect { uri: String },
    InvalidGrant,
    Forbidden,
}

/// Validate an authorization request, creating a login continuation when a
/// fresh password login is required or a consent continuation otherwise.
/// Every authorization displays consent; `prompt=none` never shows UI.
pub fn authorize(
    tx: &mut Transaction<'_>,
    host: &mut impl Host,
    authority: &impl Authority,
    config: &Config,
    req: &AuthorizeRequest<'_>,
    session: Option<&BrowserSession>,
    now: i64,
) -> Result<AuthorizeOutcome, Error> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let Some(client) = config.client(req.client_id) else {
        return Ok(AuthorizeOutcome::DirectError {
            description: "Unknown client",
        });
    };
    // Never redirect on an unrecognized URI, including error paths.
    if !client
        .redirect_uris
        .iter()
        .any(|uri| uri == req.redirect_uri)
    {
        return Ok(AuthorizeOutcome::DirectError {
            description: "Unregistered redirect URI",
        });
    }
    let error = |error: &'static str, description: &'static str| AuthorizeOutcome::RedirectError {
        redirect: req.redirect_uri.into(),
        error,
        description,
        state: req.state.into(),
    };
    if req.response_type != "code" {
        return Ok(error(
            "unsupported_response_type",
            "Only authorization code is supported",
        ));
    }
    if req.has_request {
        return Ok(error(
            "request_not_supported",
            "Request extension is not supported",
        ));
    }
    if req.has_request_uri {
        return Ok(error(
            "request_uri_not_supported",
            "Request extension is not supported",
        ));
    }
    if req.has_registration {
        return Ok(error(
            "registration_not_supported",
            "Request extension is not supported",
        ));
    }
    if !matches!(req.response_mode, None | Some("") | Some("query")) {
        return Ok(error(
            "invalid_request",
            "Only query response mode is supported",
        ));
    }
    if !valid_scope(req.scope) {
        return Ok(error(
            "invalid_scope",
            "Request openid and supported scopes",
        ));
    }
    if !valid_challenge(req.code_challenge) || req.code_challenge_method != "S256" {
        return Ok(error("invalid_request", "PKCE S256 is required"));
    }
    if req.state.len() > 1024 || req.nonce.len() > 256 {
        return Ok(error("invalid_request", "Parameter too long"));
    }
    let mut prompt_none = false;
    let mut prompt_login = false;
    let mut prompt_select = false;
    for prompt in req.prompt.split_whitespace() {
        match prompt {
            "none" => prompt_none = true,
            "login" => prompt_login = true,
            "select_account" => prompt_select = true,
            "consent" => {}
            _ => return Ok(error("invalid_request", "Invalid prompt combination")),
        }
    }
    if prompt_none && (prompt_login || prompt_select || req.prompt.split_whitespace().count() != 1)
    {
        return Ok(error("invalid_request", "Invalid prompt combination"));
    }
    let max_age = match req.max_age.filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => match raw.parse::<i64>() {
            Ok(age) if age >= 0 => Some(age),
            _ => return Ok(error("invalid_request", "Invalid max_age")),
        },
    };
    let live = match session {
        Some(candidate) => check_browser(tx, authority, candidate, now)?,
        None => None,
    };
    let reauth = match &live {
        None => true,
        Some(valid) => {
            max_age.is_some_and(|age| age == 0 || now > valid.auth_time.saturating_add(age))
                || req.hint_subject.is_some_and(|hint| hint != valid.subject)
                || prompt_login
                || prompt_select
        }
    };
    if reauth {
        if prompt_none {
            return Ok(error("login_required", "Active authentication is required"));
        }
        let handle = host.random()?;
        let data = Record {
            client: client.id.clone(),
            redirect: req.redirect_uri.into(),
            scope: req.scope.into(),
            state: req.state.into(),
            nonce: req.nonce.into(),
            challenge: req.code_challenge.into(),
            subject: req.hint_subject.unwrap_or_default().into(),
            session: live
                .as_ref()
                .map(|valid| encode_b64(&valid.session))
                .unwrap_or_default(),
            auth_time: now,
            ..Record::default()
        };
        flow_insert(
            tx,
            &host.digest(&handle),
            "login",
            &data,
            now.checked_add(CONTINUATION_SECONDS)
                .ok_or(Error::Invalid)?,
        )?;
        return Ok(AuthorizeOutcome::RequireLogin {
            handle,
            reauth: live.is_some(),
        });
    }
    let valid = live.expect("reauth covers missing session");
    if prompt_none {
        return Ok(error("consent_required", "Interactive consent is required"));
    }
    let handle = host.random()?;
    let data = Record {
        client: client.id.clone(),
        redirect: req.redirect_uri.into(),
        scope: req.scope.into(),
        state: req.state.into(),
        nonce: req.nonce.into(),
        challenge: req.code_challenge.into(),
        subject: valid.subject.clone(),
        session: encode_b64(&valid.session),
        auth_time: valid.auth_time,
        ..Record::default()
    };
    flow_insert(
        tx,
        &host.digest(&handle),
        "consent",
        &data,
        now.checked_add(CONTINUATION_SECONDS)
            .ok_or(Error::Invalid)?,
    )?;
    Ok(AuthorizeOutcome::ShowConsent { handle })
}

/// Resume a login continuation after a fresh password login. The continuation
/// is single-use: resuming deletes it and issues a consent continuation.
pub fn resume(
    tx: &mut Transaction<'_>,
    host: &mut impl Host,
    authority: &impl Authority,
    config: &Config,
    req: &ResumeRequest<'_>,
    session: Option<&BrowserSession>,
    now: i64,
) -> Result<ResumeOutcome, Error> {
    if now < 0 || req.handle.is_empty() {
        return Err(Error::Invalid);
    }
    let digest = host.digest(req.handle);
    let Some(flow) = flow_get(tx, &digest)? else {
        return Ok(ResumeOutcome::InvalidGrant);
    };
    if flow.kind != "login" || now >= flow.expires {
        flow_delete(tx, &digest)?;
        return Ok(ResumeOutcome::InvalidGrant);
    }
    let Some(candidate) = session else {
        return Ok(ResumeOutcome::InvalidGrant);
    };
    let Some(valid) = check_browser(tx, authority, candidate, now)? else {
        return Ok(ResumeOutcome::InvalidGrant);
    };
    // The continuation only completes with a *different* session than the one
    // that requested it: the browser must finish a fresh password login first.
    // It must also have authenticated at or after the continuation was created.
    // Equal timestamps permit a genuinely fresh login in the same Unix second;
    // a different session created earlier does not establish fresh authentication.
    if valid.session == decode_b64_opt(&flow.data.session)?
        || valid.auth_time < flow.data.auth_time
        || (!flow.data.subject.is_empty() && flow.data.subject != valid.subject)
    {
        return Ok(ResumeOutcome::LoginRequired);
    }
    if config.client(&flow.data.client).is_none() {
        return Ok(ResumeOutcome::InvalidGrant);
    }
    let handle = host.random()?;
    let data = Record {
        subject: valid.subject.clone(),
        session: encode_b64(&valid.session),
        auth_time: valid.auth_time,
        ..flow.data
    };
    flow_delete(tx, &digest)?;
    flow_insert(
        tx,
        &host.digest(&handle),
        "consent",
        &data,
        now.checked_add(CONTINUATION_SECONDS)
            .ok_or(Error::Invalid)?,
    )?;
    Ok(ResumeOutcome::ShowConsent { handle })
}

/// Decide a consent continuation. `allow` consumes it into a 60-second code;
/// `deny` consumes it into an `access_denied` redirect. Reuse is invalid.
pub fn consent(
    tx: &mut Transaction<'_>,
    host: &mut impl Host,
    authority: &impl Authority,
    config: &Config,
    req: &ConsentRequest<'_>,
    session: Option<&BrowserSession>,
    now: i64,
) -> Result<ConsentOutcome, Error> {
    if now < 0 || req.handle.is_empty() {
        return Err(Error::Invalid);
    }
    if req.origin != config.issuer {
        return Ok(ConsentOutcome::Forbidden);
    }
    let digest = host.digest(req.handle);
    let Some(flow) = flow_get(tx, &digest)? else {
        return Ok(ConsentOutcome::InvalidGrant);
    };
    if flow.kind != "consent" || now >= flow.expires {
        flow_delete(tx, &digest)?;
        return Ok(ConsentOutcome::InvalidGrant);
    }
    let Some(candidate) = session else {
        return Ok(ConsentOutcome::InvalidGrant);
    };
    let Some(valid) = check_browser(tx, authority, candidate, now)? else {
        return Ok(ConsentOutcome::InvalidGrant);
    };
    if valid.subject != flow.data.subject || valid.session != decode_b64_opt(&flow.data.session)? {
        return Ok(ConsentOutcome::InvalidGrant);
    }
    if config.client(&flow.data.client).is_none() {
        return Ok(ConsentOutcome::InvalidGrant);
    }
    if req.decision != "allow" {
        flow_delete(tx, &digest)?;
        return Ok(ConsentOutcome::RedirectError {
            redirect: flow.data.redirect,
            state: flow.data.state,
        });
    }
    let code = host.random()?;
    flow_delete(tx, &digest)?;
    flow_insert(
        tx,
        &host.digest(&code),
        "code",
        &flow.data,
        now.checked_add(CODE_SECONDS).ok_or(Error::Invalid)?,
    )?;
    Ok(ConsentOutcome::Redirect {
        uri: authorize_redirect(&flow.data.redirect, &code, &flow.data.state, &config.issuer),
    })
}

/// Redeem a code for tokens. Consumed-code replay deactivates the grant
/// family and still commits; the caller observes `InvalidGrant`.
pub fn exchange_code(
    tx: &mut Transaction<'_>,
    host: &mut impl Host,
    authority: &impl Authority,
    config: &Config,
    req: &ExchangeRequest<'_>,
    now: i64,
) -> Result<ExchangeOutcome, Error> {
    if now < 0 || req.code.is_empty() || req.redirect_uri.is_empty() || req.verifier.is_empty() {
        return Err(Error::Invalid);
    }
    let client = match authenticate(config, req.auth, host) {
        Ok(client) => client,
        Err(()) => return Ok(ExchangeOutcome::InvalidClient),
    };
    let digest = host.digest(req.code);
    let Some(flow) = flow_get(tx, &digest)? else {
        return Ok(ExchangeOutcome::InvalidGrant);
    };
    if flow.kind == "used_code" {
        if flow.data.client == client.id && !flow.data.grant.is_empty() {
            grant_deactivate(tx, &decode_b64(&flow.data.grant)?)?;
        }
        return Ok(ExchangeOutcome::InvalidGrant);
    }
    if flow.kind != "code" || now >= flow.expires {
        if flow.kind == "code" {
            flow_delete(tx, &digest)?;
        }
        return Ok(ExchangeOutcome::InvalidGrant);
    }
    if flow.data.client != client.id
        || flow.data.redirect != req.redirect_uri
        || !verify_pkce(host, req.verifier, &flow.data.challenge)
    {
        return Ok(ExchangeOutcome::InvalidGrant);
    }
    let session = decode_b64(&flow.data.session)?;
    let Ok(subject) = authority.subject(tx, &session, now) else {
        return Ok(ExchangeOutcome::InvalidGrant);
    };
    if subject != flow.data.subject {
        return Ok(ExchangeOutcome::InvalidGrant);
    }
    let grant_raw = host.random()?;
    let grant = host.digest(&grant_raw);
    let grant_expires = now.checked_add(GRANT_SECONDS).ok_or(Error::Invalid)?;
    let access_raw = host.random()?;
    let refresh_raw = host.random()?;
    let id_token = sign_claims(
        host,
        config,
        &flow.data,
        &session,
        req.claims,
        &access_raw,
        now,
    )?;
    flow_delete(tx, &digest)?;
    let used = Record {
        grant: encode_b64(&grant),
        ..flow.data.clone()
    };
    flow_insert(tx, &digest, "used_code", &used, grant_expires)?;
    grant_insert(tx, &grant, &flow.data, grant_expires)?;
    token_insert(
        tx,
        &host.digest(&access_raw),
        &grant,
        "access",
        now.checked_add(ACCESS_SECONDS)
            .ok_or(Error::Invalid)?
            .min(grant_expires),
    )?;
    token_insert(
        tx,
        &host.digest(&refresh_raw),
        &grant,
        "refresh",
        grant_expires,
    )?;
    Ok(ExchangeOutcome::Issued(Tokens {
        access: access_raw,
        refresh: refresh_raw,
        id_token,
        expires_in: ACCESS_SECONDS,
        scope: flow.data.scope,
    }))
}

/// Rotate a refresh token. Reuse of a consumed token revokes the whole family
/// and still commits; the caller observes `InvalidGrant`. Authentication time
/// is preserved from the original grant.
pub fn refresh(
    tx: &mut Transaction<'_>,
    host: &mut impl Host,
    authority: &impl Authority,
    config: &Config,
    req: &RefreshRequest<'_>,
    now: i64,
) -> Result<RefreshOutcome, Error> {
    if now < 0 || req.token.is_empty() {
        return Err(Error::Invalid);
    }
    let client = match authenticate(config, req.auth, host) {
        Ok(client) => client,
        Err(()) => return Ok(RefreshOutcome::InvalidClient),
    };
    let digest = host.digest(req.token);
    let Some(token) = token_get(tx, &digest)? else {
        return Ok(RefreshOutcome::InvalidGrant);
    };
    if token.kind != "refresh" || now >= token.expires {
        if token.kind == "refresh" {
            token_delete(tx, &digest)?;
        }
        return Ok(RefreshOutcome::InvalidGrant);
    }
    let Some(grant) = grant_get(tx, &token.grant)? else {
        return Ok(RefreshOutcome::InvalidGrant);
    };
    if !grant.active || now >= grant.expires {
        return Ok(RefreshOutcome::InvalidGrant);
    }
    if grant.data.client != client.id {
        return Ok(RefreshOutcome::InvalidGrant);
    }
    if token.used {
        // A consumed token signals family replay. Revoke successors too, and
        // commit that revocation alongside the protocol error.
        grant_deactivate(tx, &token.grant)?;
        return Ok(RefreshOutcome::InvalidGrant);
    }
    if req.scope.is_some_and(|scope| scope != grant.data.scope) {
        return Ok(RefreshOutcome::InvalidScope);
    }
    let session = decode_b64(&grant.data.session)?;
    let Ok(subject) = authority.subject(tx, &session, now) else {
        return Ok(RefreshOutcome::InvalidGrant);
    };
    if subject != grant.data.subject {
        return Ok(RefreshOutcome::InvalidGrant);
    }
    let access_raw = host.random()?;
    let refresh_raw = host.random()?;
    let id_token = sign_claims(
        host,
        config,
        &grant.data,
        &session,
        req.claims,
        &access_raw,
        now,
    )?;
    token_mark_used(tx, &digest)?;
    token_insert(
        tx,
        &host.digest(&access_raw),
        &token.grant,
        "access",
        now.checked_add(ACCESS_SECONDS)
            .ok_or(Error::Invalid)?
            .min(grant.expires),
    )?;
    token_insert(
        tx,
        &host.digest(&refresh_raw),
        &token.grant,
        "refresh",
        grant.expires,
    )?;
    Ok(RefreshOutcome::Issued(Tokens {
        access: access_raw,
        refresh: refresh_raw,
        id_token,
        expires_in: ACCESS_SECONDS,
        scope: grant.data.scope,
    }))
}

/// Validate an access token and return scope-filtered profile claims. No
/// writes are staged; the grant, token, and session are rechecked in this
/// transaction.
pub fn userinfo(
    tx: &mut Transaction<'_>,
    host: &impl Host,
    authority: &impl Authority,
    req: &UserinfoRequest<'_>,
    now: i64,
) -> Result<UserinfoOutcome, Error> {
    if now < 0 || req.token.is_empty() {
        return Err(Error::Invalid);
    }
    let digest = host.digest(req.token);
    let Some(token) = token_get(tx, &digest)? else {
        return Ok(UserinfoOutcome::InvalidToken);
    };
    if token.kind != "access" || now >= token.expires {
        return Ok(UserinfoOutcome::InvalidToken);
    }
    let Some(grant) = grant_get(tx, &token.grant)? else {
        return Ok(UserinfoOutcome::InvalidToken);
    };
    if !grant.active || now >= grant.expires {
        return Ok(UserinfoOutcome::InvalidToken);
    }
    let session = decode_b64(&grant.data.session)?;
    let Ok(subject) = authority.subject(tx, &session, now) else {
        return Ok(UserinfoOutcome::InvalidToken);
    };
    if subject != grant.data.subject {
        return Ok(UserinfoOutcome::InvalidToken);
    }
    Ok(UserinfoOutcome::Claims(scoped_claims(
        &grant.data.subject,
        &grant.data.scope,
        req.claims,
    )))
}

/// Revoke the complete grant family holding a token. Unknown tokens and
/// cross-client tokens still report `Revoked` without writes.
pub fn revoke(
    tx: &mut Transaction<'_>,
    host: &impl Host,
    config: &Config,
    req: &RevokeRequest<'_>,
    now: i64,
) -> Result<RevokeOutcome, Error> {
    if now < 0 || req.token.is_empty() {
        return Err(Error::Invalid);
    }
    let client = match authenticate(config, req.auth, host) {
        Ok(client) => client,
        Err(()) => return Ok(RevokeOutcome::InvalidClient),
    };
    let digest = host.digest(req.token);
    let Some(token) = token_get(tx, &digest)? else {
        return Ok(RevokeOutcome::Revoked);
    };
    let Some(grant) = grant_get(tx, &token.grant)? else {
        return Ok(RevokeOutcome::Revoked);
    };
    if grant.data.client != client.id || !grant.active || now >= grant.expires {
        return Ok(RevokeOutcome::Revoked);
    }
    grant_deactivate(tx, &token.grant)?;
    Ok(RevokeOutcome::Revoked)
}

/// Begin RP-initiated logout. Always confirms when a session is present,
/// including absent hints and hints for another login, so a cross-site GET
/// can never end the browser session. Without a session, redirects at once.
pub fn logout(
    tx: &mut Transaction<'_>,
    host: &mut impl Host,
    authority: &impl Authority,
    config: &Config,
    req: &LogoutRequest<'_>,
    now: i64,
) -> Result<LogoutOutcome, Error> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let client_id = req
        .client_id
        .filter(|s| !s.is_empty())
        .or(req.hint.as_ref().map(|hint| hint.aud.as_str()));
    let client = match client_id {
        None => None,
        Some(id) => match config.client(id) {
            Some(client) => Some(client),
            None => {
                return Ok(LogoutOutcome::DirectError {
                    description: "Unknown client",
                });
            }
        },
    };
    if let Some(hint) = &req.hint
        && client.is_some_and(|client| hint.aud != client.id)
    {
        return Ok(LogoutOutcome::DirectError {
            description: "Invalid logout client",
        });
    }
    let redirect = req
        .post_logout_redirect_uri
        .filter(|s| !s.is_empty())
        .unwrap_or("/");
    if redirect != "/"
        && client.is_none_or(|client| {
            !client
                .post_logout_redirect_uris
                .iter()
                .any(|uri| uri == redirect)
        })
    {
        return Ok(LogoutOutcome::DirectError {
            description: "Unregistered logout redirect",
        });
    }
    let Some(candidate) = req.session else {
        return Ok(LogoutOutcome::Redirect {
            uri: logout_redirect(redirect, req.state),
        });
    };
    let Some(valid) = check_browser(tx, authority, candidate, now)? else {
        return Ok(LogoutOutcome::Redirect {
            uri: logout_redirect(redirect, req.state),
        });
    };
    let handle = host.random()?;
    flow_insert(
        tx,
        &host.digest(&handle),
        "logout",
        &Record {
            client: client.map(|c| c.id.clone()).unwrap_or_default(),
            redirect: redirect.into(),
            state: req.state.into(),
            subject: valid.subject.clone(),
            session: encode_b64(&valid.session),
            auth_time: valid.auth_time,
            ..Record::default()
        },
        now.checked_add(CONTINUATION_SECONDS)
            .ok_or(Error::Invalid)?,
    )?;
    Ok(LogoutOutcome::ShowConfirm { handle })
}

/// Confirm logout. The host ends the Identity session in the same transaction
/// after `Redirect`; grants die with the session, so no grant writes are
/// needed here.
pub fn logout_confirm(
    tx: &mut Transaction<'_>,
    host: &impl Host,
    config: &Config,
    req: &LogoutConfirm<'_>,
    session: Option<&BrowserSession>,
    now: i64,
) -> Result<LogoutConfirmOutcome, Error> {
    if now < 0 || req.handle.is_empty() {
        return Err(Error::Invalid);
    }
    if req.origin != config.issuer {
        return Ok(LogoutConfirmOutcome::Forbidden);
    }
    let digest = host.digest(req.handle);
    let Some(flow) = flow_get(tx, &digest)? else {
        return Ok(LogoutConfirmOutcome::InvalidGrant);
    };
    if flow.kind != "logout" || now >= flow.expires {
        flow_delete(tx, &digest)?;
        return Ok(LogoutConfirmOutcome::InvalidGrant);
    }
    let valid = match session {
        Some(candidate) if !candidate.session.is_empty() => candidate,
        _ => return Ok(LogoutConfirmOutcome::InvalidGrant),
    };
    if valid.subject != flow.data.subject || valid.session != decode_b64_opt(&flow.data.session)? {
        return Ok(LogoutConfirmOutcome::InvalidGrant);
    }
    flow_delete(tx, &digest)?;
    Ok(LogoutConfirmOutcome::Redirect {
        uri: logout_redirect(&flow.data.redirect, &flow.data.state),
    })
}

/// Host-only subject lookup for a raw code or bearer token.
///
/// Digests `token` and returns the recorded grant subject when the digest
/// matches a `code`/`used_code` flow or an `access`/`refresh` token whose
/// grant row is present. Continuation handles (`login`/`consent`/`logout`)
/// and unknown digests report `None`.
///
/// This is a read-only helper so hosts can fetch fresh profile [`Claims`]
/// for the grant subject in the SAME transaction before calling
/// [`exchange_code`], [`refresh`], or [`userinfo`]. It promises no authority
/// or validity: expiry, revocation, session liveness, and single-use state
/// are still enforced by those calls, which alone govern success. No writes
/// are staged. Residency misses propagate as `Err` and stay terminal.
pub fn token_subject(
    tx: &mut Transaction<'_>,
    host: &impl Host,
    token: &str,
) -> Result<Option<String>, Error> {
    if token.is_empty() {
        return Err(Error::Invalid);
    }
    let digest = host.digest(token);
    if let Some(flow) = flow_get(tx, &digest)? {
        if flow.kind == "code" || flow.kind == "used_code" {
            return Ok(Some(flow.data.subject));
        }
        return Ok(None);
    }
    if let Some(found) = token_get(tx, &digest)? {
        if found.kind == "access" || found.kind == "refresh" {
            let Some(grant) = grant_get(tx, &found.grant)? else {
                return Ok(None);
            };
            return Ok(Some(grant.data.subject));
        }
        return Ok(None);
    }
    Ok(None)
}

/// Host-only consent summary for a raw continuation handle.
///
/// Digests `handle` and returns `(client_id, scope, redirect_uri)` when it matches a live
/// `consent` continuation, so the host can name the requesting app (via its
/// static registry) and the requested scopes on the explicit-consent page
/// instead of rendering a blind approve prompt. Other kinds (`login`,
/// `code`, `used_code`, `logout`) and unknown digests report `None`.
///
/// Read-only like [`token_subject`]: no liveness promise beyond the recorded
/// kind, no writes, misses propagate. The subsequent [`consent`] decision
/// still enforces single-use and expiry.
pub fn consent_details(
    tx: &mut Transaction<'_>,
    host: &impl Host,
    handle: &str,
) -> Result<Option<(String, String, String)>, Error> {
    if handle.is_empty() {
        return Err(Error::Invalid);
    }
    let digest = host.digest(handle);
    let Some(flow) = flow_get(tx, &digest)? else {
        return Ok(None);
    };
    if flow.kind != "consent" {
        return Ok(None);
    }
    Ok(Some((
        flow.data.client,
        flow.data.scope,
        flow.data.redirect,
    )))
}

/// Delete expired continuations, codes, tokens, and grants. Hosts call this
/// after loading complete tables; it requires complete residency like any
/// other scan.
pub fn prune_expired(tx: &mut Transaction<'_>, now: i64) -> Result<usize, Error> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let mut removed = 0;
    for row in tx.find(FLOWS, "primary", &[])? {
        if integer(&row, "expires")? <= now {
            tx.delete(FLOWS, &[row_key(&row, "id")?])?;
            removed += 1;
        }
    }
    for row in tx.find(TOKENS, "primary", &[])? {
        if integer(&row, "expires")? <= now {
            tx.delete(TOKENS, &[row_key(&row, "id")?])?;
            removed += 1;
        }
    }
    for row in tx.find(GRANTS, "primary", &[])? {
        if integer(&row, "expires")? <= now {
            tx.delete(GRANTS, &[row_key(&row, "id")?])?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Record {
    #[serde(default)]
    grant: String,
    #[serde(default)]
    client: String,
    #[serde(default)]
    redirect: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    challenge: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    session: String,
    #[serde(default)]
    auth_time: i64,
}

struct Flow {
    kind: String,
    data: Record,
    expires: i64,
}

struct StoredToken {
    grant: Vec<u8>,
    kind: String,
    used: bool,
    expires: i64,
}

struct StoredGrant {
    data: Record,
    active: bool,
    expires: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ValidSession {
    subject: String,
    session: Vec<u8>,
    auth_time: i64,
}

fn check_browser(
    tx: &mut Transaction<'_>,
    authority: &impl Authority,
    candidate: &BrowserSession,
    now: i64,
) -> Result<Option<ValidSession>, Error> {
    if candidate.subject.is_empty() || candidate.session.is_empty() || candidate.auth_time < 0 {
        return Err(Error::Invalid);
    }
    match authority.subject(tx, &candidate.session, now) {
        Ok(subject) => {
            if subject != candidate.subject {
                return Ok(None);
            }
            Ok(Some(ValidSession {
                subject: candidate.subject.clone(),
                session: candidate.session.clone(),
                auth_time: candidate.auth_time,
            }))
        }
        Err(Error::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

fn authenticate<'a>(
    config: &'a Config,
    auth: &ClientAuth<'_>,
    host: &impl Host,
) -> Result<&'a Client, ()> {
    if auth.body_had_secret || auth.client_id.is_empty() {
        return Err(());
    }
    let client = config.client(auth.client_id).ok_or(())?;
    match (&client.secret_digest, auth.secret) {
        (Some(expected), Some(provided)) if constant_time_eq(expected, &host.digest(provided)) => {
            Ok(client)
        }
        (None, None) => Ok(client),
        _ => Err(()),
    }
}

fn valid_scope(scope: &str) -> bool {
    let mut openid = false;
    for part in scope.split_whitespace() {
        match part {
            "openid" => openid = true,
            "profile" | "email" => {}
            _ => return false,
        }
    }
    openid
}

fn valid_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn verify_pkce(host: &impl Host, verifier: &str, challenge: &str) -> bool {
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
    {
        return false;
    }
    let computed = URL_SAFE_NO_PAD.encode(host.digest(verifier));
    constant_time_eq_str(&computed, challenge)
}

fn sign_claims(
    host: &impl Host,
    config: &Config,
    data: &Record,
    session: &[u8],
    claims: &Claims,
    access: &str,
    now: i64,
) -> Result<String, Error> {
    let digest = host.digest(access);
    if digest.len() < 16 {
        return Err(Error::Invalid);
    }
    if session.is_empty() || data.subject.is_empty() {
        return Err(Error::Invalid);
    }
    let mut value = scoped_claims(&data.subject, &data.scope, claims);
    value["iss"] = config.issuer.clone().into();
    value["aud"] = data.client.clone().into();
    value["iat"] = now.into();
    value["exp"] = now
        .checked_add(ACCESS_SECONDS)
        .ok_or(Error::Invalid)?
        .into();
    value["auth_time"] = data.auth_time.into();
    // Public correlation is domain- and client-separated from the private
    // Identity lookup handle. No internal digest enters the signed payload.
    value["sid"] = encode_b64(&host.digest(&alloc::format!(
        "snap-oidc/public-session/v1\0{}\0{}\0{}",
        config.issuer,
        data.client,
        encode_b64(session)
    )))
    .into();
    value["at_hash"] = URL_SAFE_NO_PAD.encode(&digest[..16]).into();
    if !data.nonce.is_empty() {
        value["nonce"] = data.nonce.clone().into();
    }
    host.sign(&value)
}

fn scoped_claims(subject: &str, scope: &str, claims: &Claims) -> serde_json::Value {
    let mut value = serde_json::json!({"sub": subject});
    for part in scope.split_whitespace() {
        match part {
            "profile" => {
                value["name"] = claims.name.clone().into();
                // No wall-clock source, no claim: a non-positive timestamp
                // means the profile store keeps none and is omitted.
                if claims.updated_at > 0 {
                    value["updated_at"] = claims.updated_at.into();
                }
            }
            "email" => {
                value["email"] = claims.email.clone().into();
                value["email_verified"] = claims.email_verified.into();
            }
            _ => {}
        }
    }
    value
}

fn authorize_redirect(base: &str, code: &str, state: &str, issuer: &str) -> String {
    alloc::format!(
        "{base}{sep}code={code}&state={state}&iss={issuer}",
        sep = if base.contains('?') { "&" } else { "?" },
        code = query_value(code),
        state = query_value(state),
        issuer = query_value(issuer),
    )
}

fn logout_redirect(base: &str, state: &str) -> String {
    alloc::format!(
        "{base}{sep}state={state}",
        sep = if base.contains('?') { "&" } else { "?" },
        state = query_value(state),
    )
}

fn query_value(value: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 15) as usize] as char);
        }
    }
    encoded
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn constant_time_eq_str(left: &str, right: &str) -> bool {
    constant_time_eq(left.as_bytes(), right.as_bytes())
}

fn encode_b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_b64(raw: &str) -> Result<Vec<u8>, Error> {
    URL_SAFE_NO_PAD.decode(raw).map_err(|_| Error::Invalid)
}

fn decode_b64_opt(raw: &str) -> Result<Vec<u8>, Error> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    decode_b64(raw)
}

fn flow_get(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<Option<Flow>, Error> {
    let Some(row) = tx.get(FLOWS, &[Value::Bytes(digest.into())])? else {
        return Ok(None);
    };
    Ok(Some(Flow {
        kind: text(&row, "kind")?.into(),
        data: record(&row)?,
        expires: integer(&row, "expires")?,
    }))
}

fn flow_insert(
    tx: &mut Transaction<'_>,
    digest: &[u8],
    kind: &str,
    data: &Record,
    expires: i64,
) -> Result<(), Error> {
    tx.insert(
        FLOWS,
        row([
            ("id", Value::Bytes(digest.into())),
            ("kind", kind.into()),
            (
                "data",
                serde_json::to_string(data)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
            ("expires", expires.into()),
        ]),
    )
}

fn flow_delete(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<(), Error> {
    tx.delete(FLOWS, &[Value::Bytes(digest.into())]).map(|_| ())
}

fn grant_get(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<Option<StoredGrant>, Error> {
    let Some(row) = tx.get(GRANTS, &[Value::Bytes(digest.into())])? else {
        return Ok(None);
    };
    Ok(Some(StoredGrant {
        data: record(&row)?,
        active: integer(&row, "active")? == 1,
        expires: integer(&row, "expires")?,
    }))
}

fn grant_insert(
    tx: &mut Transaction<'_>,
    digest: &[u8],
    data: &Record,
    expires: i64,
) -> Result<(), Error> {
    tx.insert(
        GRANTS,
        row([
            ("id", Value::Bytes(digest.into())),
            (
                "data",
                serde_json::to_string(data)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
            ("active", 1i64.into()),
            ("expires", expires.into()),
        ]),
    )
}

fn grant_deactivate(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<(), Error> {
    let key = Value::Bytes(digest.into());
    let Some(row) = tx.get(GRANTS, core::slice::from_ref(&key))? else {
        return Ok(());
    };
    if integer(&row, "active")? != 1 {
        return Ok(());
    }
    let mut changes = Row::new();
    changes.insert("active".into(), 0i64.into());
    tx.update(GRANTS, &[key], changes).map(|_| ())
}

fn token_get(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<Option<StoredToken>, Error> {
    let Some(row) = tx.get(TOKENS, &[Value::Bytes(digest.into())])? else {
        return Ok(None);
    };
    Ok(Some(StoredToken {
        grant: bytes(&row, "grant")?,
        kind: text(&row, "kind")?.into(),
        used: integer(&row, "used")? == 1,
        expires: integer(&row, "expires")?,
    }))
}

fn token_insert(
    tx: &mut Transaction<'_>,
    digest: &[u8],
    grant: &[u8],
    kind: &str,
    expires: i64,
) -> Result<(), Error> {
    tx.insert(
        TOKENS,
        row([
            ("id", Value::Bytes(digest.into())),
            ("grant", Value::Bytes(grant.into())),
            ("kind", kind.into()),
            ("used", 0i64.into()),
            ("expires", expires.into()),
        ]),
    )
}

fn token_mark_used(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<(), Error> {
    let key = Value::Bytes(digest.into());
    tx.get(TOKENS, core::slice::from_ref(&key))?
        .ok_or(Error::Invalid)?;
    let mut changes = Row::new();
    changes.insert("used".into(), 1i64.into());
    tx.update(TOKENS, &[key], changes).map(|_| ())
}

fn token_delete(tx: &mut Transaction<'_>, digest: &[u8]) -> Result<(), Error> {
    tx.delete(TOKENS, &[Value::Bytes(digest.into())])
        .map(|_| ())
}

fn row<const N: usize>(fields: [(&str, Value); N]) -> Row {
    fields
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
}

fn record(row: &Row) -> Result<Record, Error> {
    serde_json::from_str(text(row, "data")?).map_err(|_| Error::Invalid)
}

fn text<'a>(row: &'a Row, key: &str) -> Result<&'a str, Error> {
    match row.get(key) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(Error::Invalid),
    }
}

fn integer(row: &Row, key: &str) -> Result<i64, Error> {
    match row.get(key) {
        Some(Value::Integer(value)) => Ok(*value),
        _ => Err(Error::Invalid),
    }
}

fn bytes(row: &Row, key: &str) -> Result<Vec<u8>, Error> {
    match row.get(key) {
        Some(Value::Bytes(value)) => Ok(value.clone()),
        _ => Err(Error::Invalid),
    }
}

fn row_key(row: &Row, key: &str) -> Result<Value, Error> {
    row.get(key).cloned().ok_or(Error::Invalid)
}
