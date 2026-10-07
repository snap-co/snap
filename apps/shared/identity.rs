//! App-host assembly shared by Chatty and Factorio. Identity owns the domain
//! operations/controllers; these helpers supply clocks, runtime, config and HTTP
//! encodings. No OAuth code exchange or rotation is implemented by this module.
#[path = "login.rs"]
mod browser;
use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use snap_identity::{Crypto, oauth as rp};
use snap_store::{Error, Host, Transaction};
pub use snap_transport::native::web::Cookies;
use snap_transport::{native::Transactions, runtime::Loop};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Unix clock")
        .as_secs() as i64
}
pub fn random() -> String {
    URL_SAFE_NO_PAD.encode(snap_crypto::Native.random().expect("host entropy"))
}
pub fn origin(value: &str) -> Result<String, Error> {
    let url = url::Url::parse(value).map_err(|_| Error::Invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
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
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret_ref: snap_config::SecretRef,
}
impl Settings {
    pub fn validate(&self) -> Result<(), Error> {
        origin(&self.issuer)?;
        if self.client_id.is_empty() || self.client_secret_ref.name().is_empty() {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn resolve(
        &self,
        origin: String,
        secrets: &snap_config::Secrets,
        dev_origins: Vec<String>,
    ) -> Result<Config, Box<dyn std::error::Error>> {
        self.validate()?;
        let secret = secrets.resolve(&self.client_secret_ref)?.clone();
        if secret.expose().len() < 32 {
            return Err("OAuth client secret must contain at least 32 bytes".into());
        }
        Ok(Config {
            origin,
            issuer: self.issuer.clone(),
            client: self.client_id.clone(),
            secret,
            dev_origins,
        })
    }
}
pub struct Config {
    pub origin: String,
    pub issuer: String,
    pub client: String,
    pub secret: snap_config::Secret,
    pub dev_origins: Vec<String>,
}
impl Config {
    pub fn provider(&self) -> Result<rp::acquisition::Provider, Error> {
        let mut continuations = BTreeMap::new();
        for value in core::iter::once(&self.origin).chain(self.dev_origins.iter()) {
            if origin(value)? != *value {
                return Err(Error::Invalid);
            }
            continuations.insert(value.clone(), format!("{value}/auth/callback"));
        }
        let issuer = origin(&self.issuer)?;
        Ok(rp::acquisition::Provider {
            name: "authy".into(),
            authorization_endpoint: format!("{issuer}/oauth/authorize"),
            issuer,
            client: self.client.clone(),
            continuations,
        })
    }
    pub fn definitions(&self) -> Result<Vec<snap_transport::operation::Definition>, Error> {
        let p = self.provider()?;
        let mut definitions = rp::acquisition::definitions(
            snap_identity::Identity::default(),
            vec![p.clone()],
            || snap_crypto::Native,
        )?
        .preconnection;
        let returns = p
            .continuations
            .keys()
            .map(|name| (name.clone(), format!("{name}/auth/logged-out")))
            .collect();
        definitions.extend(rp::release::definitions(vec![p], returns, || {
            snap_crypto::Native
        })?);
        Ok(definitions)
    }
    pub fn controllers<B: snap_store::Backend>(
        &self,
    ) -> Result<Vec<snap_transport::host::Controller<B>>, Box<dyn std::error::Error>> {
        let p = self.provider()?;
        let client = snap_http::native::Client::new()?;
        let secret = self.secret.clone();
        let runtime = tokio::runtime::Handle::current();
        let acquisition = rp::verification::controller(
            vec![p.clone()],
            client.clone(),
            || snap_crypto::Native,
            move |_| Ok(secret.expose().into()),
            now,
            move |future| runtime.block_on(future),
        )?;
        let secret = self.secret.clone();
        let runtime = tokio::runtime::Handle::current();
        let renewal = rp::verification::renewal_controller(
            vec![p],
            client,
            || snap_crypto::Native,
            move |_| Ok(secret.expose().into()),
            now,
            move |future| runtime.block_on(future),
        )?;
        Ok(vec![acquisition, renewal])
    }
    pub fn operations(
        &self,
        cookies: Cookies,
    ) -> Result<Vec<snap_transport::native::web::HttpOperation>, Error> {
        Ok(browser::operations(
            cookies,
            &self.provider()?,
            &self.origin,
            &self.dev_origins,
        ))
    }
}

/// Existing app credential preparation, kept separate from connection policy.
/// A verified browser/native credential selects the private ID. Host transactions
/// commit the renewal request and run Identity's controller before resolving it.
pub struct OAuth<H: Loop> {
    pub host: Transactions<H>,
    pub cookies: Cookies,
}
impl<H: Loop + Host + Send + 'static> OAuth<H> {
    pub fn new(
        host: impl Into<Transactions<H>>,
        cookies: Cookies,
        mut config: Config,
    ) -> Result<Arc<Self>, Error> {
        config.origin = origin(&config.origin)?;
        config.issuer = origin(&config.issuer)?;
        if config.client.is_empty() || config.secret.expose().len() < 32 {
            return Err(Error::Invalid);
        }
        config.provider()?;
        Ok(Arc::new(Self {
            host: host.into(),
            cookies,
        }))
    }
    pub fn run<T>(
        &self,
        name: &str,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.host.run(name, f)
    }
    pub async fn run_async<T: Send + 'static>(
        self: &Arc<Self>,
        name: &'static str,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let oauth = self.clone();
        tokio::task::spawn_blocking(move || oauth.run(name, f))
            .await
            .map_err(|_| Error::Unavailable)?
    }
    pub async fn session(self: &Arc<Self>, headers: &HeaderMap) -> Result<rp::Grant, Error> {
        let bearer = self.cookies.read(headers, false).ok_or(Error::NotFound)?;
        self.session_id(&rp::digest(&bearer)).await
    }
    pub async fn session_id(self: &Arc<Self>, id: &str) -> Result<rp::Grant, Error> {
        let id = id.to_owned();
        let selected = id.clone();
        self.run_async("oauth.prepare", move |tx| {
            rp::renewal::request(tx, &selected, now())
        })
        .await?;
        self.run_async("oauth.session", move |tx| rp::resolve_id(tx, &id, now()))
            .await
    }
}
pub fn no_store(value: serde_json::Value) -> Response {
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
        Json(serde_json::json!({"error":status.as_str()})),
    )
        .into_response()
}
