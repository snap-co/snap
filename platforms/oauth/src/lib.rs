//! Native OAuth relying-party IO shared by local applications. Upstream tokens
//! never reach the browser. All Store calls use the application's serialized host.
use axum::{
    Json, Router,
    extract::{RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rsa::{BigUint, RsaPublicKey, signature::Verifier};
use serde_json::{Value, json};
use sha2::Sha256;
use snap_document_local::web::{ReadCookie, Shared};
use snap_oidc::relying_party as rp;
use snap_store::{Error, Store, Transaction};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MIGRATION: &str = include_str!("../migrations/0002_oauth_host.toml");
pub const KEY_TABLE: &str = "oauth_host.keys";
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Unix clock")
        .as_secs() as i64
}
pub fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn origin(value: &str) -> Result<String, Error> {
    let url = url::Url::parse(value).map_err(|_| Error::Invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::Invalid);
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(Clone)]
pub struct Cookies {
    key: Vec<u8>,
    name: String,
    secure: bool,
}
impl Cookies {
    pub fn load(
        store: &mut Store<snap_sqlite::Sqlite>,
        application: &str,
        secure: bool,
    ) -> Result<Self, Error> {
        if application.is_empty()
            || !application
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            return Err(Error::Invalid);
        }
        store.load(KEY_TABLE)?;
        let key = store
            .run("oauth.keys", |tx| {
                if let Some(row) = tx.get(KEY_TABLE, &["cookie".into()])? {
                    return match &row["key"] {
                        snap_store::Value::Bytes(bytes) => Ok(bytes.clone()),
                        _ => Err(Error::Invalid),
                    };
                }
                let mut bytes = vec![0; 32];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                tx.insert(
                    KEY_TABLE,
                    [
                        ("id".into(), "cookie".into()),
                        ("key".into(), snap_store::Value::Bytes(bytes.clone())),
                    ]
                    .into_iter()
                    .collect(),
                )?;
                Ok(bytes)
            })?
            .value;
        Ok(Self {
            key,
            name: application.into(),
            secure,
        })
    }
    fn name(&self, correlation: bool) -> String {
        format!(
            "{}{}_{}",
            if self.secure { "__Host-" } else { "" },
            self.name,
            if correlation { "login" } else { "session" }
        )
    }
    pub fn encode(&self, bearer: Option<&str>, correlation: bool) -> String {
        let name = self.name(correlation);
        let value = bearer
            .map(|b| {
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC key");
                mac.update(name.as_bytes());
                mac.update(b.as_bytes());
                format!(
                    "{b}.{}",
                    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
                )
            })
            .unwrap_or_default();
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
            if bearer.is_none() {
                0
            } else if correlation {
                300
            } else {
                2592000
            },
            if self.secure { "; Secure" } else { "" }
        )
    }
    pub fn read(&self, headers: &HeaderMap, correlation: bool) -> Option<String> {
        let name = self.name(correlation);
        let mut found = None;
        for header in headers.get_all("cookie") {
            for part in header.to_str().ok()?.split(';') {
                let Some((key, value)) = part.trim().split_once('=') else {
                    continue;
                };
                if key != name {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                let (bearer, signature) = value.split_once('.')?;
                if bearer.len() != 43
                    || !bearer
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                {
                    return None;
                }
                let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
                mac.update(name.as_bytes());
                mac.update(bearer.as_bytes());
                mac.verify_slice(&signature).ok()?;
                found = Some(bearer.into());
            }
        }
        found
    }
    pub fn reader(&self) -> ReadCookie {
        let cookies = self.clone();
        Arc::new(move |headers| cookies.read(headers, false))
    }
}

pub struct Config {
    pub origin: String,
    pub issuer: String,
    pub client: String,
    pub secret: String,
}
pub struct OAuth {
    pub documents: Arc<Shared<snap_sqlite::Sqlite>>,
    pub cookies: Cookies,
    pub config: Config,
    http: reqwest::Client,
    refresh: tokio::sync::Mutex<()>,
    dev_origins: Vec<String>,
}
impl OAuth {
    pub fn new(
        documents: Arc<Shared<snap_sqlite::Sqlite>>,
        cookies: Cookies,
        mut config: Config,
    ) -> Result<Arc<Self>, Error> {
        config.origin = origin(&config.origin)?;
        config.issuer = origin(&config.issuer)?;
        if config.secret.len() < 32 || config.client.is_empty() {
            return Err(Error::Invalid);
        }
        let dev_origins: Vec<String> = if std::env::var("SNAP_DEV_MODE").as_deref() == Ok("1") {
            serde_json::from_str(&std::env::var("SNAP_DEV_ORIGINS").map_err(|_| Error::Invalid)?)
                .map_err(|_| Error::Invalid)?
        } else {
            Vec::new()
        };
        for value in &dev_origins {
            if origin(value)? != *value || !value.starts_with("http://") {
                return Err(Error::Invalid);
            }
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| Error::Unavailable)?;
        documents
            .host
            .lock()
            .unwrap()
            .transact("oauth.recover", |tx| rp::recover(tx, now()))?;
        Ok(Arc::new(Self {
            documents,
            cookies,
            config,
            http,
            refresh: tokio::sync::Mutex::new(()),
            dev_origins,
        }))
    }
    pub fn run<T>(
        &self,
        name: &str,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.documents.host.lock().unwrap().transact(name, f)
    }
    fn endpoint(&self, value: &Value, key: &str) -> Result<String, Error> {
        let value = value[key].as_str().ok_or(Error::Invalid)?;
        let url = url::Url::parse(value).map_err(|_| Error::Invalid)?;
        if url.origin().ascii_serialization() != self.config.issuer
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        Ok(value.into())
    }
    async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value, Error> {
        let mut response = request.send().await.map_err(|_| Error::Unavailable)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Error::NotFound);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err(Error::Invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    }
    async fn discovery(&self) -> Result<Value, Error> {
        let value = self
            .json(self.http.get(format!(
                "{}/.well-known/openid-configuration",
                self.config.issuer
            )))
            .await?;
        if value["issuer"] != self.config.issuer {
            return Err(Error::Invalid);
        }
        for key in [
            "authorization_endpoint",
            "token_endpoint",
            "jwks_uri",
            "userinfo_endpoint",
            "end_session_endpoint",
        ] {
            self.endpoint(&value, key)?;
        }
        Ok(value)
    }
    fn basic(&self) -> String {
        let encode = |value: &str| {
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("v", value)
                .finish()[2..]
                .to_string()
        };
        format!(
            "Basic {}",
            STANDARD.encode(format!(
                "{}:{}",
                encode(&self.config.client),
                encode(&self.config.secret)
            ))
        )
    }
    async fn tokens(
        &self,
        metadata: &Value,
        form: &[(&str, &str)],
        nonce: Option<&str>,
        previous: Option<&rp::Session>,
    ) -> Result<(rp::Tokens, Value), Error> {
        let response = self
            .json(
                self.http
                    .post(self.endpoint(metadata, "token_endpoint")?)
                    .header("authorization", self.basic())
                    .form(form),
            )
            .await?;
        let jwks = self
            .json(self.http.get(self.endpoint(metadata, "jwks_uri")?))
            .await?;
        let claims = verify(response["id_token"].as_str().ok_or(Error::Invalid)?, &jwks)?;
        let tokens = rp::validate_tokens(
            &response,
            &claims,
            rp::Validation {
                issuer: &self.config.issuer,
                client: &self.config.client,
                nonce,
                previous,
                now: now(),
            },
        )?;
        Ok((tokens, claims))
    }
    /// Refresh IO is serialized separately from Store. Mutation authority is still
    /// rechecked in the application's own transaction immediately before its write.
    pub async fn session(&self, headers: &HeaderMap) -> Result<rp::Session, Error> {
        let bearer = self.cookies.read(headers, false).ok_or(Error::NotFound)?;
        let _refresh = self.refresh.lock().await;
        let previous = self.run("oauth.refresh.begin", |tx| {
            rp::begin_refresh(tx, &bearer, now())
        })?;
        if let Some(previous) = previous {
            let result = async {
                let metadata = self.discovery().await?;
                let (tokens, _) = self
                    .tokens(
                        &metadata,
                        &[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", &previous.tokens.refresh),
                        ],
                        None,
                        Some(&previous),
                    )
                    .await?;
                self.run("oauth.refresh.finish", |tx| {
                    rp::finish_refresh(tx, &previous, tokens, now())
                })
            }
            .await;
            if result.is_err() {
                let _ = self.run("oauth.refresh.failed", |tx| rp::revoke(tx, &bearer));
            }
            result
        } else {
            self.run("oauth.session", |tx| rp::resolve(tx, &bearer, now()))
        }
    }
    pub fn csrf(&self, headers: &HeaderMap, session: &rp::Session) -> Result<(), Error> {
        if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(&self.config.origin)
            || headers
                .get("x-snap-csrf")
                .and_then(|v| v.to_str().ok())
                .is_none_or(|v| !rp::same_secret(v, &session.csrf))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    // Only snap dev supplies this header after checking the incoming Host and
    // Origin pair. Packaged hosts ignore it; dev hosts also enforce the startup
    // allowlist. The issuer and token validation never change with the browser URL.
    fn browser_origin<'a>(&'a self, headers: &HeaderMap) -> Result<&'a str, Error> {
        if self.dev_origins.is_empty() {
            return Ok(&self.config.origin);
        }
        match headers.get("x-snap-dev-origin") {
            None => Ok(&self.config.origin),
            Some(value) => self
                .dev_origins
                .iter()
                .find(|origin| value.to_str().ok() == Some(origin.as_str()))
                .map(String::as_str)
                .ok_or(Error::Invalid),
        }
    }
    async fn login(&self, headers: &HeaderMap) -> Result<Response, Error> {
        let origin = self.browser_origin(headers)?;
        let metadata = self.discovery().await?;
        let state = random();
        let binding = random();
        let attempt = rp::Attempt {
            binding: rp::digest(&binding),
            nonce: random(),
            verifier: random(),
            redirect: format!("{origin}/auth/callback"),
            issuer: self.config.issuer.clone(),
            old_session: self.cookies.read(headers, false).map(|b| rp::digest(&b)),
            logout: false,
            expires: now() + 300,
            processing: false,
        };
        let old = self.cookies.read(headers, true);
        self.run("oauth.login", |tx| {
            rp::clear_attempts(tx, old.as_deref(), now())?;
            rp::start(tx, &state, &attempt)
        })?;
        let mut target = url::Url::parse(&self.endpoint(&metadata, "authorization_endpoint")?)
            .map_err(|_| Error::Invalid)?;
        target.query_pairs_mut().extend_pairs([
            ("client_id", self.config.client.as_str()),
            ("redirect_uri", &attempt.redirect),
            ("response_type", "code"),
            ("scope", "openid profile email"),
            ("state", &state),
            ("nonce", &attempt.nonce),
            ("code_challenge", &rp::digest(&attempt.verifier)),
            ("code_challenge_method", "S256"),
        ]);
        let mut response = redirect(target.as_str())?;
        cookie(&mut response, self.cookies.encode(Some(&binding), true))?;
        Ok(response)
    }
    async fn callback(
        &self,
        headers: &HeaderMap,
        params: BTreeMap<String, String>,
    ) -> Result<Response, Error> {
        let state = params.get("state").ok_or(Error::Invalid)?;
        let binding = self.cookies.read(headers, true).ok_or(Error::NotFound)?;
        let attempt = self.run("oauth.code.begin", |tx| {
            let attempt = rp::consume(tx, state, &binding, false, now())?;
            if attempt.redirect != format!("{}/auth/callback", self.browser_origin(headers)?) {
                return Err(Error::Invalid);
            }
            Ok(attempt)
        })?;
        if params.contains_key("error")
            || params.get("iss") != Some(&self.config.issuer)
            || attempt.issuer != self.config.issuer
        {
            return Err(Error::Invalid);
        }
        let code = params.get("code").ok_or(Error::Invalid)?;
        let metadata = self.discovery().await?;
        let (tokens, claims) = self
            .tokens(
                &metadata,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", &attempt.redirect),
                    ("code_verifier", &attempt.verifier),
                ],
                Some(&attempt.nonce),
                None,
            )
            .await?;
        let profile = self
            .json(
                self.http
                    .get(self.endpoint(&metadata, "userinfo_endpoint")?)
                    .bearer_auth(&tokens.access),
            )
            .await?;
        if profile["sub"] != claims["sub"] {
            return Err(Error::Invalid);
        }
        let bearer = random();
        let subject = claims["sub"].as_str().ok_or(Error::Invalid)?;
        let session = rp::Session {
            id: rp::digest(&bearer),
            owner: rp::owner(&self.config.issuer, subject),
            subject: subject.into(),
            issuer: self.config.issuer.clone(),
            csrf: random(),
            nonce: attempt.nonce,
            profile,
            tokens,
            expires: now() + 2592000,
            refreshing: false,
            version: 1,
        };
        self.run("oauth.code.finish", |tx| {
            rp::issue(tx, state, &session, now())
        })?;
        let mut response = redirect("/")?;
        cookie(&mut response, self.cookies.encode(Some(&bearer), false))?;
        cookie(&mut response, self.cookies.encode(None, true))?;
        Ok(response)
    }
    fn logout(&self, headers: &HeaderMap) -> Result<Response, Error> {
        let redirect_uri = format!("{}/auth/logged-out", self.browser_origin(headers)?);
        let bearer = self.cookies.read(headers, false).ok_or(Error::NotFound)?;
        let old = self.cookies.read(headers, true);
        let state = random();
        let binding = random();
        self.run("oauth.logout", |tx| {
            let session = rp::for_logout(tx, &bearer, now())?;
            self.csrf(headers, &session)?;
            rp::revoke(tx, &bearer)?;
            rp::clear_attempts(tx, old.as_deref(), now())?;
            rp::start(
                tx,
                &state,
                &rp::Attempt {
                    binding: rp::digest(&binding),
                    nonce: String::new(),
                    verifier: String::new(),
                    redirect: redirect_uri.clone(),
                    issuer: self.config.issuer.clone(),
                    old_session: None,
                    logout: true,
                    expires: now() + 300,
                    processing: false,
                },
            )?;
            Ok(())
        })?;
        let mut target = url::Url::parse(&format!("{}/oauth/logout", self.config.issuer))
            .map_err(|_| Error::Invalid)?;
        target.query_pairs_mut().extend_pairs([
            ("client_id", self.config.client.as_str()),
            ("post_logout_redirect_uri", &redirect_uri),
            ("state", &state),
        ]);
        let mut response = no_store(json!({"redirect":target.as_str()}));
        cookie(&mut response, self.cookies.encode(None, false))?;
        cookie(&mut response, self.cookies.encode(Some(&binding), true))?;
        Ok(response)
    }
    fn logged_out(
        &self,
        headers: &HeaderMap,
        params: BTreeMap<String, String>,
    ) -> Result<Response, Error> {
        let state = params.get("state").ok_or(Error::Invalid)?;
        let binding = self.cookies.read(headers, true).ok_or(Error::NotFound)?;
        self.run("oauth.logout.finish", |tx| {
            let attempt = rp::consume(tx, state, &binding, true, now())?;
            if attempt.redirect != format!("{}/auth/logged-out", self.browser_origin(headers)?) {
                return Err(Error::Invalid);
            }
            rp::finish_logout(tx, state)
        })?;
        let mut response = redirect("/")?;
        cookie(&mut response, self.cookies.encode(None, true))?;
        Ok(response)
    }
    pub fn routes(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/auth/login", get(login))
            .route("/auth/callback", get(callback))
            .route("/auth/logout", post(logout))
            .route("/auth/logged-out", get(logged_out))
            .with_state(self.clone())
    }
}

/// Verify only a unique RSA signing key selected by kid. Token-controlled jku/x5u
/// are ignored; the caller fetches keys from the pinned issuer discovery document.
pub fn verify(token: &str, jwks: &Value) -> Result<Value, Error> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::Invalid);
    }
    let decode = |part: &str| URL_SAFE_NO_PAD.decode(part).map_err(|_| Error::Invalid);
    let header: Value = serde_json::from_slice(&decode(parts[0])?).map_err(|_| Error::Invalid)?;
    if header["alg"] != "RS256" || header.get("crit").is_some() {
        return Err(Error::Invalid);
    }
    let kid = header["kid"].as_str().ok_or(Error::Invalid)?;
    let keys: Vec<_> = jwks["keys"]
        .as_array()
        .ok_or(Error::Invalid)?
        .iter()
        .filter(|key| key["kid"].as_str() == Some(kid))
        .collect();
    if keys.len() != 1 {
        return Err(Error::Invalid);
    }
    let key = keys[0];
    if key["kty"] != "RSA"
        || key.get("alg").is_some_and(|v| v != "RS256")
        || key.get("use").is_some_and(|v| v != "sig")
        || key.get("key_ops").is_some_and(|v| {
            v.as_array()
                .is_none_or(|ops| !ops.iter().any(|op| op == "verify"))
        })
    {
        return Err(Error::Invalid);
    }
    let n = BigUint::from_bytes_be(&decode(key["n"].as_str().ok_or(Error::Invalid)?)?);
    let e = BigUint::from_bytes_be(&decode(key["e"].as_str().ok_or(Error::Invalid)?)?);
    if n.bits() < 2048 {
        return Err(Error::Invalid);
    }
    let public = RsaPublicKey::new(n, e).map_err(|_| Error::Invalid)?;
    let signature = decode(parts[2])?;
    let signature =
        rsa::pkcs1v15::Signature::try_from(signature.as_slice()).map_err(|_| Error::Invalid)?;
    rsa::pkcs1v15::VerifyingKey::<Sha256>::new(public)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .map_err(|_| Error::Invalid)?;
    serde_json::from_slice(&decode(parts[1])?).map_err(|_| Error::Invalid)
}
pub fn no_store(value: Value) -> Response {
    ([("cache-control", "no-store")], Json(value)).into_response()
}
pub fn failure(error: Error) -> Response {
    let status = match error {
        Error::NotFound => StatusCode::UNAUTHORIZED,
        Error::Constraint => StatusCode::CONFLICT,
        Error::Invalid => StatusCode::BAD_REQUEST,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        [("cache-control", "no-store")],
        Json(json!({"error":status.as_str()})),
    )
        .into_response()
}
fn redirect(target: &str) -> Result<Response, Error> {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert("location", target.parse().map_err(|_| Error::Invalid)?);
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    Ok(response)
}
fn cookie(response: &mut Response, value: String) -> Result<(), Error> {
    response
        .headers_mut()
        .append("set-cookie", value.parse().map_err(|_| Error::Invalid)?);
    Ok(())
}
fn params(query: Option<String>) -> Result<BTreeMap<String, String>, Error> {
    let mut result = BTreeMap::new();
    for (k, v) in url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes()) {
        if result.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(Error::Invalid);
        }
    }
    Ok(result)
}
async fn login(State(app): State<Arc<OAuth>>, headers: HeaderMap) -> Response {
    app.login(&headers).await.unwrap_or_else(failure)
}
async fn callback(
    State(app): State<Arc<OAuth>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    match params(query) {
        Ok(p) => app.callback(&headers, p).await.unwrap_or_else(failure),
        Err(e) => failure(e),
    }
}
async fn logout(State(app): State<Arc<OAuth>>, headers: HeaderMap) -> Response {
    app.logout(&headers).unwrap_or_else(failure)
}
async fn logged_out(
    State(app): State<Arc<OAuth>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    match params(query) {
        Ok(p) => app.logged_out(&headers, p).unwrap_or_else(failure),
        Err(e) => failure(e),
    }
}
