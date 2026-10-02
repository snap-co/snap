//! Standard HTTP carrier for the portable issuer. Store transactions include
//! session validation, protocol state, fresh profile claims and token issuance.
use crate::pages::escape;
use crate::{App, json_response, now, store_error};
use axum::{
    Router,
    body::Bytes,
    extract::{RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use snap_identity::{Crypto, Identity};
use snap_oidc as oidc;
use snap_store::{Error, Transaction};
use std::{collections::BTreeMap, sync::Arc};

pub struct Issuer {
    pub config: oidc::Config,
}
impl Issuer {
    pub fn new(
        origin: &str,
        settings: &crate::config::Settings,
        secrets: &snap_config::Secrets,
        origins: &BTreeMap<String, Vec<String>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        settings.validate()?;
        let mut clients = Vec::new();
        for configured in &settings.clients {
            let rp = settings.client_origin(configured)?;
            let rp = url::Url::parse(&rp)?.origin().ascii_serialization();
            let mut client = oidc::Client::public(
                &configured.id,
                &configured.name,
                &format!("{rp}/auth/callback"),
                &format!("{rp}/auth/logged-out"),
            )?;
            if let Some(reference) = &configured.client_secret_ref {
                let secret = secrets.resolve(reference)?;
                if secret.expose().len() < 32 {
                    return Err("OAuth client secret must contain at least 32 bytes".into());
                }
                client.secret_digest = Some(Sha256::digest(secret.expose().as_bytes()).to_vec());
            }
            clients.push(client);
        }
        {
            for client in &mut clients {
                for value in origins.get(&client.id).into_iter().flatten() {
                    let url = url::Url::parse(value)?;
                    if !matches!(url.scheme(), "http" | "https")
                        || url.origin().ascii_serialization() != *value
                    {
                        return Err("Invalid development client origin".into());
                    }
                    for (uris, path) in [
                        (&mut client.redirect_uris, "callback"),
                        (&mut client.post_logout_redirect_uris, "logged-out"),
                    ] {
                        let uri = format!("{value}/auth/{path}");
                        if !uris.contains(&uri) {
                            uris.push(uri);
                        }
                    }
                }
            }
        }
        configure_auto_approval(&mut clients, &settings.auto_approve_domain)?;
        Ok(Self {
            config: oidc::Config {
                issuer: origin.into(),
                clients,
            },
        })
    }
}

// Approval is derived only from registered callbacks, never Host, Origin or a
// caller's claimed client name. Empty configuration disables automatic consent.
fn configure_auto_approval(
    clients: &mut [oidc::Client],
    domain: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !domain.is_empty() {
        domain_app_origin(domain, "validate")?;
    }
    let domain = domain.to_ascii_lowercase();
    let suffix = format!(".{domain}");
    for client in clients {
        client.preapproved_redirect_uris.clear();
        for uri in &client.redirect_uris {
            let url = url::Url::parse(uri)?;
            if !domain.is_empty()
                && url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && url
                    .host_str()
                    .is_some_and(|host| host == domain || host.ends_with(&suffix))
            {
                client.preapproved_redirect_uris.push(uri.clone());
            }
        }
    }
    Ok(())
}

// A configured domain derives one exact origin per registered client. It never
// registers arbitrary request-supplied subdomains or makes clients interchangeable.
pub(crate) fn domain_app_origin(
    domain: &str,
    app: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if app.is_empty()
        || app.len() > 63
        || app.starts_with('-')
        || app.ends_with('-')
        || !app.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        || domain.len() > 253
        || !domain.contains('.')
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(
            "Configured domain must be a DNS domain without scheme, port or wildcard".into(),
        );
    }
    Ok(format!("https://{app}.{}", domain.to_ascii_lowercase()))
}

struct CryptoHost<'a>(&'a crate::keys::Keys);
impl oidc::Host for CryptoHost<'_> {
    fn random(&mut self) -> Result<String, Error> {
        Ok(crate::keys::random())
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        Sha256::digest(secret.as_bytes()).to_vec()
    }
    fn sign(&self, claims: &Value) -> Result<String, Error> {
        self.0.sign(claims)
    }
}
struct Authority;
impl oidc::Authority for Authority {
    fn subject(&self, tx: &mut Transaction<'_>, session: &[u8], now: i64) -> Result<String, Error> {
        Identity::default()
            .resolve_digest(tx, session, now)
            .map(|s| s.identity)
    }
}
type Params = BTreeMap<String, String>;
struct HttpError(StatusCode, &'static str);
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        problem(self.0, self.1)
    }
}
fn parse(text: &str) -> Result<Params, HttpError> {
    let mut result = Params::new();
    for (key, value) in url::form_urlencoded::parse(text.as_bytes()) {
        if result
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(HttpError(StatusCode::BAD_REQUEST, "invalid_request"));
        }
    }
    Ok(result)
}
fn form(headers: &HeaderMap, bytes: &[u8]) -> Result<Params, HttpError> {
    if headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        != Some("application/x-www-form-urlencoded")
    {
        return Err(HttpError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_request",
        ));
    }
    parse(
        std::str::from_utf8(bytes)
            .map_err(|_| HttpError(StatusCode::BAD_REQUEST, "invalid_request"))?,
    )
}
fn value<'a>(params: &'a Params, key: &str) -> &'a str {
    params.get(key).map(String::as_str).unwrap_or("")
}
fn problem(status: StatusCode, error: &str) -> Response {
    let mut response = json_response(status, json!({"error":error}));
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert("www-authenticate", "Bearer".parse().unwrap());
    }
    response
}
fn redirect(uri: &str) -> Response {
    match uri.parse::<axum::http::HeaderValue>() {
        Ok(location) => {
            let mut response = StatusCode::SEE_OTHER.into_response();
            response.headers_mut().insert("location", location);
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
        Err(_) => problem(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}
fn redirect_error(app: &App, uri: &str, error: &str, state: &str) -> Response {
    let Ok(mut uri) = url::Url::parse(uri) else {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    };
    uri.query_pairs_mut()
        .append_pair("error", error)
        .append_pair("state", state)
        .append_pair("iss", &app.origin);
    redirect(uri.as_str())
}
fn page(app: &App, title: &str, body: String) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{} · Authy</title><link rel=\"stylesheet\" href=\"/style.css\"></head><body>{body}</body></html>",
        escape(title)
    );
    // Browsers enforce form-action on the redirect following a consent POST too.
    // Only configured relying-party origins may receive that navigation. The issuer
    // still validates the exact registered redirect URI before issuing a code.
    let mut actions = String::from("'self'");
    for client in &app.issuer.config.clients {
        for uri in client
            .redirect_uris
            .iter()
            .chain(&client.post_logout_redirect_uris)
        {
            if let Ok(uri) = url::Url::parse(uri)
                && matches!(uri.scheme(), "http" | "https")
            {
                actions.push(' ');
                actions.push_str(&uri.origin().ascii_serialization());
            }
        }
    }
    let policy = format!(
        "default-src 'none'; style-src 'self'; form-action {actions}; frame-ancestors 'none'; base-uri 'none'"
    );
    // no-referrer makes navigation POSTs send Origin: null in Chromium, preventing
    // the issuer's CSRF check. same-origin preserves it without leaking flow URLs.
    (
        [
            ("content-security-policy", policy.as_str()),
            ("cache-control", "no-store"),
            ("referrer-policy", "same-origin"),
        ],
        Html(html),
    )
        .into_response()
}
fn browser(
    app: &App,
    tx: &mut Transaction<'_>,
    headers: &HeaderMap,
) -> Result<Option<oidc::BrowserSession>, Error> {
    let Some(bearer) = app.bearer(headers) else {
        return Ok(None);
    };
    match Identity::default().resolve(tx, &snap_crypto::Native, &bearer, now()) {
        Ok(session) => Ok(Some(oidc::BrowserSession {
            subject: session.identity,
            session: snap_crypto::Native.digest(&bearer),
            auth_time: session.authenticated_at,
        })),
        Err(Error::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}
fn consent_page(app: &App, headers: &HeaderMap, handle: String) -> Response {
    let details = app.run("oauth.consent_details", |tx| {
        let session = browser(app, tx, headers)?.ok_or(Error::NotFound)?;
        let email = authy::profile_info(tx, &session.subject)?.email;
        Ok(oidc::consent_details(tx, &CryptoHost(&app.keys), &handle)?
            .map(|(client, scope, redirect)| (client, scope, redirect, email)))
    });
    let (client, scope, redirect, email) = match details {
        Ok(Some(details)) => details,
        Ok(None) => return problem(StatusCode::BAD_REQUEST, "invalid_grant"),
        Err(error) => return store_error(error),
    };
    let name = app
        .issuer
        .config
        .client(&client)
        .map(|c| c.name.as_str())
        .unwrap_or(&client);
    let origin = url::Url::parse(&redirect)
        .ok()
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    page(
        app,
        "Authorize application",
        app.pages.consent(name, &origin, &email, &scope, &handle),
    )
}
fn hint(app: &App, raw: &str) -> Result<Value, HttpError> {
    let claims = app
        .keys
        .verify_hint(raw)
        .map_err(|_| HttpError(StatusCode::BAD_REQUEST, "invalid_request"))?;
    if claims["iss"] != app.origin
        || claims["sub"].as_str().is_none_or(str::is_empty)
        || claims["sid"].as_str().is_none_or(str::is_empty)
        || claims["aud"]
            .as_str()
            .and_then(|id| app.issuer.config.client(id))
            .is_none()
    {
        return Err(HttpError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    Ok(claims)
}
async fn discovery(State(app): State<Arc<App>>) -> Response {
    json_response(StatusCode::OK, oidc::discovery(&app.issuer.config))
}
async fn jwks(State(app): State<Arc<App>>) -> Response {
    json_response(StatusCode::OK, app.keys.jwks())
}

async fn authorize(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match parse(query.as_deref().unwrap_or("")) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    let hint_claims = match params.get("id_token_hint") {
        Some(raw) => match hint(&app, raw) {
            Ok(h) => Some(h),
            Err(r) => return r.into_response(),
        },
        None => None,
    };
    if hint_claims
        .as_ref()
        .is_some_and(|hint| hint["aud"] != value(&params, "client_id"))
    {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let result = app.run("oauth.authorize", |tx| {
        let session = browser(&app, tx, &headers)?;
        oidc::authorize(
            tx,
            &mut CryptoHost(&app.keys),
            &Authority,
            &app.issuer.config,
            &oidc::AuthorizeRequest {
                client_id: value(&params, "client_id"),
                redirect_uri: value(&params, "redirect_uri"),
                response_type: value(&params, "response_type"),
                scope: value(&params, "scope"),
                state: value(&params, "state"),
                nonce: value(&params, "nonce"),
                code_challenge: value(&params, "code_challenge"),
                code_challenge_method: value(&params, "code_challenge_method"),
                prompt: value(&params, "prompt"),
                max_age: params.get("max_age").map(String::as_str),
                hint_subject: hint_claims.as_ref().and_then(|v| v["sub"].as_str()),
                response_mode: params.get("response_mode").map(String::as_str),
                has_request: params.contains_key("request"),
                has_request_uri: params.contains_key("request_uri"),
                has_registration: params.contains_key("registration"),
            },
            session.as_ref(),
            now(),
        )
    });
    match result {
        Ok(oidc::AuthorizeOutcome::Redirect { uri }) => redirect(&uri),
        Ok(oidc::AuthorizeOutcome::ShowConsent { handle }) => consent_page(&app, &headers, handle),
        Ok(oidc::AuthorizeOutcome::RequireLogin { handle, reauth }) => {
            let resume = format!(
                "/oauth/resume?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("request", &handle)
                    .finish()
            );
            redirect(&format!(
                "/?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("continue", &resume)
                    .append_pair("reauth", if reauth { "1" } else { "0" })
                    .finish()
            ))
        }
        Ok(oidc::AuthorizeOutcome::RedirectError {
            redirect: uri,
            error,
            state,
            ..
        }) => redirect_error(&app, &uri, error, &state),
        Ok(oidc::AuthorizeOutcome::DirectError { .. }) => {
            problem(StatusCode::BAD_REQUEST, "invalid_request")
        }
        Err(error) => store_error(error),
    }
}
async fn resume(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match parse(query.as_deref().unwrap_or("")) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    match app.run("oauth.resume", |tx| {
        let session = browser(&app, tx, &headers)?;
        oidc::resume(
            tx,
            &mut CryptoHost(&app.keys),
            &Authority,
            &app.issuer.config,
            &oidc::ResumeRequest {
                handle: value(&params, "request"),
            },
            session.as_ref(),
            now(),
        )
    }) {
        Ok(oidc::ResumeOutcome::Redirect { uri }) => redirect(&uri),
        Ok(oidc::ResumeOutcome::ShowConsent { handle }) => consent_page(&app, &headers, handle),
        Ok(_) => problem(StatusCode::BAD_REQUEST, "login_required"),
        Err(error) => store_error(error),
    }
}
async fn consent(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let params = match form(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    match app.run("oauth.consent", |tx| {
        let session = browser(&app, tx, &headers)?;
        oidc::consent(
            tx,
            &mut CryptoHost(&app.keys),
            &Authority,
            &app.issuer.config,
            &oidc::ConsentRequest {
                handle: value(&params, "request"),
                decision: value(&params, "decision"),
                origin: headers
                    .get("origin")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
            },
            session.as_ref(),
            now(),
        )
    }) {
        Ok(oidc::ConsentOutcome::Redirect { uri }) => redirect(&uri),
        Ok(oidc::ConsentOutcome::RedirectError {
            redirect: uri,
            state,
        }) => redirect_error(&app, &uri, "access_denied", &state),
        Ok(oidc::ConsentOutcome::Forbidden) => StatusCode::FORBIDDEN.into_response(),
        Ok(oidc::ConsentOutcome::InvalidGrant) => problem(StatusCode::BAD_REQUEST, "invalid_grant"),
        Err(error) => store_error(error),
    }
}

struct ClientAuth {
    id: String,
    secret: Option<String>,
    body_had_secret: bool,
}
impl ClientAuth {
    fn borrowed(&self) -> oidc::ClientAuth<'_> {
        oidc::ClientAuth {
            client_id: &self.id,
            secret: self.secret.as_deref(),
            body_had_secret: self.body_had_secret,
        }
    }
}
fn client_auth(headers: &HeaderMap, params: &Params) -> Result<ClientAuth, HttpError> {
    let mut auth = headers.get_all("authorization").iter();
    let header = auth.next();
    if auth.next().is_some() {
        return Err(HttpError(StatusCode::UNAUTHORIZED, "invalid_client"));
    }
    let (id, secret) = if let Some(header) = header {
        let value = header
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Basic "))
            .ok_or(HttpError(StatusCode::UNAUTHORIZED, "invalid_client"))?;
        let bytes = STANDARD
            .decode(value)
            .map_err(|_| HttpError(StatusCode::UNAUTHORIZED, "invalid_client"))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| HttpError(StatusCode::UNAUTHORIZED, "invalid_client"))?;
        let (id, secret) = text
            .split_once(':')
            .ok_or(HttpError(StatusCode::UNAUTHORIZED, "invalid_client"))?;
        let decode = |s: &str| {
            url::form_urlencoded::parse(format!("v={s}").as_bytes())
                .next()
                .unwrap()
                .1
                .into_owned()
        };
        (decode(id), Some(decode(secret)))
    } else {
        (value(params, "client_id").into(), None)
    };
    if params.get("client_id").is_some_and(|given| given != &id) {
        return Err(HttpError(StatusCode::UNAUTHORIZED, "invalid_client"));
    }
    Ok(ClientAuth {
        id,
        secret,
        body_had_secret: params.contains_key("client_secret"),
    })
}
fn claims(app: &App, tx: &mut Transaction<'_>, token: &str) -> Result<oidc::Claims, Error> {
    let Some(subject) = oidc::token_subject(tx, &CryptoHost(&app.keys), token)? else {
        return Ok(oidc::Claims {
            name: String::new(),
            email: String::new(),
            email_verified: false,
            updated_at: 0,
        });
    };
    let account = authy::profile_info(tx, &subject)?;
    let document = authy::document().read(tx, &account.profile, Some(&subject))?;
    Ok(oidc::Claims {
        name: document.value["name"]
            .as_str()
            .ok_or(Error::Invalid)?
            .into(),
        email: account.email,
        email_verified: false,
        updated_at: 0,
    })
}
fn tokens(tokens: oidc::Tokens) -> Response {
    json_response(
        StatusCode::OK,
        json!({"access_token":tokens.access,"refresh_token":tokens.refresh,"id_token":tokens.id_token,"expires_in":tokens.expires_in,"scope":tokens.scope,"token_type":"Bearer"}),
    )
}
async fn token(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let params = match form(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    let auth = match client_auth(&headers, &params) {
        Ok(a) => a,
        Err(r) => return r.into_response(),
    };
    match value(&params, "grant_type") {
        "authorization_code" => match app.run("oauth.code", |tx| {
            let claims = claims(&app, tx, value(&params, "code"))?;
            oidc::exchange_code(
                tx,
                &mut CryptoHost(&app.keys),
                &Authority,
                &app.issuer.config,
                &oidc::ExchangeRequest {
                    auth: &auth.borrowed(),
                    code: value(&params, "code"),
                    redirect_uri: value(&params, "redirect_uri"),
                    verifier: value(&params, "code_verifier"),
                    claims: &claims,
                },
                now(),
            )
        }) {
            Ok(oidc::ExchangeOutcome::Issued(value)) => tokens(value),
            Ok(oidc::ExchangeOutcome::InvalidClient) => {
                problem(StatusCode::UNAUTHORIZED, "invalid_client")
            }
            Ok(oidc::ExchangeOutcome::InvalidGrant) => {
                problem(StatusCode::BAD_REQUEST, "invalid_grant")
            }
            Err(error) => store_error(error),
        },
        "refresh_token" => match app.run("oauth.refresh", |tx| {
            let claims = claims(&app, tx, value(&params, "refresh_token"))?;
            oidc::refresh(
                tx,
                &mut CryptoHost(&app.keys),
                &Authority,
                &app.issuer.config,
                &oidc::RefreshRequest {
                    auth: &auth.borrowed(),
                    token: value(&params, "refresh_token"),
                    scope: params.get("scope").map(String::as_str),
                    claims: &claims,
                },
                now(),
            )
        }) {
            Ok(oidc::RefreshOutcome::Issued(value)) => tokens(value),
            Ok(oidc::RefreshOutcome::InvalidClient) => {
                problem(StatusCode::UNAUTHORIZED, "invalid_client")
            }
            Ok(oidc::RefreshOutcome::InvalidScope) => {
                problem(StatusCode::BAD_REQUEST, "invalid_scope")
            }
            Ok(oidc::RefreshOutcome::InvalidGrant) => {
                problem(StatusCode::BAD_REQUEST, "invalid_grant")
            }
            Err(error) => store_error(error),
        },
        _ => problem(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
    }
}
async fn userinfo(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if headers.get_all("authorization").iter().count() != 1 {
        return problem(StatusCode::UNAUTHORIZED, "invalid_token");
    }
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return problem(StatusCode::UNAUTHORIZED, "invalid_token");
    };
    match app.run("oauth.userinfo", |tx| {
        let claims = claims(&app, tx, token)?;
        oidc::userinfo(
            tx,
            &CryptoHost(&app.keys),
            &Authority,
            &oidc::UserinfoRequest {
                token,
                claims: &claims,
            },
            now(),
        )
    }) {
        Ok(oidc::UserinfoOutcome::Claims(claims)) => json_response(StatusCode::OK, claims),
        Ok(oidc::UserinfoOutcome::InvalidToken) => {
            problem(StatusCode::UNAUTHORIZED, "invalid_token")
        }
        Err(error) => store_error(error),
    }
}
async fn revoke(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let params = match form(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    let auth = match client_auth(&headers, &params) {
        Ok(a) => a,
        Err(r) => return r.into_response(),
    };
    match app.run("oauth.revoke", |tx| {
        oidc::revoke(
            tx,
            &CryptoHost(&app.keys),
            &app.issuer.config,
            &oidc::RevokeRequest {
                auth: &auth.borrowed(),
                token: value(&params, "token"),
            },
            now(),
        )
    }) {
        Ok(oidc::RevokeOutcome::Revoked) => json_response(StatusCode::OK, json!({})),
        Ok(oidc::RevokeOutcome::InvalidClient) => {
            problem(StatusCode::UNAUTHORIZED, "invalid_client")
        }
        Err(error) => store_error(error),
    }
}
async fn logout_get(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match parse(query.as_deref().unwrap_or("")) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    logout(app, headers, params)
}
async fn logout_post(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let params = match form(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r.into_response(),
    };
    logout(app, headers, params)
}
fn logout(app: Arc<App>, headers: HeaderMap, params: Params) -> Response {
    if let Some(handle) = params.get("request") {
        let result = app.run("oauth.logout_confirm", |tx| {
            let session = browser(&app, tx, &headers)?;
            let result = oidc::logout_confirm(
                tx,
                &CryptoHost(&app.keys),
                &app.issuer.config,
                &oidc::LogoutConfirm {
                    handle,
                    origin: headers
                        .get("origin")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or(""),
                    session: session.as_ref(),
                },
                session.as_ref(),
                now(),
            )?;
            if matches!(result, oidc::LogoutConfirmOutcome::Redirect { .. })
                && let Some(bearer) = app.bearer(&headers)
            {
                Identity::default().revoke(tx, &snap_crypto::Native, &bearer, now())?;
            }
            Ok(result)
        });
        return match result {
            Ok(oidc::LogoutConfirmOutcome::Redirect { uri }) => {
                let mut response = redirect(&uri);
                response
                    .headers_mut()
                    .insert("set-cookie", app.keys.cookie(None).parse().unwrap());
                response
            }
            Ok(oidc::LogoutConfirmOutcome::Forbidden) => StatusCode::FORBIDDEN.into_response(),
            Ok(oidc::LogoutConfirmOutcome::InvalidGrant) => {
                problem(StatusCode::BAD_REQUEST, "invalid_grant")
            }
            Err(error) => store_error(error),
        };
    }
    let hint = match params.get("id_token_hint") {
        Some(raw) => match hint(&app, raw) {
            Ok(claims) => Some(oidc::IdHint {
                aud: claims["aud"].as_str().unwrap().into(),
            }),
            Err(r) => return r.into_response(),
        },
        None => None,
    };
    match app.run("oauth.logout", |tx| {
        let session = browser(&app, tx, &headers)?;
        oidc::logout(
            tx,
            &mut CryptoHost(&app.keys),
            &Authority,
            &app.issuer.config,
            &oidc::LogoutRequest {
                client_id: params.get("client_id").map(String::as_str),
                post_logout_redirect_uri: params
                    .get("post_logout_redirect_uri")
                    .map(String::as_str),
                hint: hint.as_ref(),
                state: value(&params, "state"),
                session: session.as_ref(),
            },
            now(),
        )
    }) {
        Ok(oidc::LogoutOutcome::ShowConfirm { handle }) => {
            page(&app, "Sign out of Authy", app.pages.logout(&handle))
        }
        Ok(oidc::LogoutOutcome::Redirect { uri }) => redirect(&uri),
        Ok(oidc::LogoutOutcome::DirectError { .. }) => {
            problem(StatusCode::BAD_REQUEST, "invalid_request")
        }
        Err(error) => store_error(error),
    }
}

async fn browser_errors(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let wants_html = request
        .headers()
        .get("accept")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    let response = next.run(request).await;
    let status = response.status();
    if !wants_html || !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let message = if status.is_server_error() {
        "Authy is temporarily unavailable. Please try again shortly."
    } else if status == StatusCode::FORBIDDEN {
        "This request could not be verified. Your account has not been changed."
    } else {
        "This request is invalid or has expired. Start again from the application you want to use."
    };
    let mut response = page(&app, "Request unsuccessful", app.pages.error(message));
    *response.status_mut() = status;
    response
}

pub fn routes(app: Arc<App>) -> Router<Arc<App>> {
    let browser = Router::new()
        .route("/oauth/authorize", get(authorize).post(consent))
        .route("/oauth/resume", get(resume))
        .route("/oauth/logout", get(logout_get).post(logout_post))
        .route_layer(axum::middleware::from_fn_with_state(app, browser_errors));
    Router::new()
        .merge(browser)
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/oauth/jwks", get(jwks))
        .route("/oauth/token", axum::routing::post(token))
        .route("/oauth/userinfo", get(userinfo).post(userinfo))
        .route("/oauth/revoke", axum::routing::post(revoke))
}

#[cfg(test)]
mod domain_tests {
    use super::{configure_auto_approval, domain_app_origin};
    #[test]
    fn auto_approval_selects_only_registered_https_domain_callbacks() {
        let approved = [
            "https://snapco.dev/auth/callback",
            "https://factorio.snapco.dev/auth/callback",
            "https://app.team.snapco.dev/auth/callback",
        ];
        let denied = [
            "http://factorio.snapco.dev/auth/callback",
            "https://notsnapco.dev/auth/callback",
            "https://snapco.dev.evil.test/auth/callback",
            "https://snapco.dev@evil.test/auth/callback",
            "https://evil.test/auth/callback?next=https://snapco.dev",
            "https://user@app.snapco.dev/auth/callback",
            "https://app.snapco.dev/auth/callback#fragment",
            "http://127.0.0.1:3852/auth/callback",
        ];
        let mut client = snap_oidc::Client::public("app", "App", approved[0], "").unwrap();
        client.redirect_uris = approved
            .iter()
            .chain(&denied)
            .map(|s| (*s).into())
            .collect();
        let mut clients = [client];
        configure_auto_approval(&mut clients, "Snapco.dev").unwrap();
        assert_eq!(clients[0].preapproved_redirect_uris, approved);
        configure_auto_approval(&mut clients, "").unwrap();
        assert!(clients[0].preapproved_redirect_uris.is_empty());
        assert!(configure_auto_approval(&mut clients, "*.snapco.dev").is_err());
    }

    #[test]
    fn derives_separate_exact_https_origins_and_rejects_url_components() {
        assert_eq!(
            domain_app_origin("CC.example.test", "factorio").unwrap(),
            "https://factorio.cc.example.test"
        );
        assert_eq!(
            domain_app_origin("cc.example.test", "chatty").unwrap(),
            "https://chatty.cc.example.test"
        );
        for domain in [
            "",
            "localhost",
            "https://cc.example.test",
            "*.example.test",
            "example.test:443",
            "example.test/path",
            "example.test@evil.test",
            ".example.test",
            "-cc.example.test",
        ] {
            assert!(domain_app_origin(domain, "factorio").is_err());
        }
    }
}
