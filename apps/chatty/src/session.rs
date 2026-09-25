//! Server-side OIDC relying party. Browser correlation, OAuth tokens and local
//! session authority have separate lifetimes. A lost code/refresh result requires
//! a fresh login; it is never retried with a consumed credential.
use crate::{Config, Host, storage::*};
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use snap_http::{
    Cookie, Request, Response,
    client::{Outgoing, collect},
    query,
};
use snap_store::{Guard, Predicate as P, Statement as S, Store};

#[derive(Clone)]
pub struct Sessions<S, H, K> {
    pub store: S,
    pub host: H,
    pub config: Config,
    pub cookie: K,
    pub correlation: K,
}
#[derive(Clone)]
pub struct Actor {
    pub id: String,
    pub owner: String,
    pub subject: String,
    pub csrf: String,
    pub version: i64,
    pub expires: u64,
    pub data: Value,
}
impl Actor {
    /// An admitted turn survives routine token refresh, but cannot outlive local
    /// logout or session expiry. Refresh version is not an authorization identity.
    pub fn lease(&self, now: u64) -> Guard {
        let mut q = live(SESSIONS, &self.id, now);
        q.filter.push(P::eq("owner", self.owner.clone()));
        guard(q)
    }
    pub fn authority(&self, now: u64) -> Guard {
        let mut q = live(SESSIONS, &self.id, now);
        q.filter.extend([
            P::eq("owner", self.owner.clone()),
            P::eq("version", self.version),
            P::eq("refreshing", 0i64),
        ]);
        guard(q)
    }
    pub fn public(&self) -> Value {
        json!({"identified":true,"account":{"id":self.subject,"name":self.data["profile"]["name"],"email":self.data["profile"]["email"]},"csrf":self.csrf})
    }
}
impl<D: Store, H: Host, K: Cookie> Sessions<D, H, K> {
    async fn remote(
        &self,
        method: &'static str,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<Value, Response> {
        if !url.starts_with(&format!("{}/", self.config.issuer)) {
            return Err(Response::error(
                502,
                "issuer_error",
                "Issuer endpoint is outside its configured origin",
            ));
        }
        let mut response = self
            .host
            .send(Outgoing {
                method,
                url,
                headers,
                body,
                max_bytes: 256 * 1024,
                timeout_ms: 15_000,
            })
            .await
            .map_err(|_| Response::error(502, "issuer_error", "Authy could not be reached"))?;
        let body = collect(&mut response.body, 256 * 1024)
            .await
            .map_err(|_| Response::error(502, "issuer_error", "Invalid Authy response"))?;
        if response.status != 200 {
            return Err(Response::error(
                401,
                "login_required",
                "Authy did not accept the login grant. Sign in again.",
            ));
        }
        serde_json::from_slice(&body)
            .map_err(|_| Response::error(502, "issuer_error", "Invalid Authy JSON"))
    }
    async fn discovery(&self) -> Result<Value, Response> {
        let value = self
            .remote(
                "GET",
                format!("{}/.well-known/openid-configuration", self.config.issuer),
                vec![],
                vec![],
            )
            .await?;
        if value["issuer"].as_str() != Some(&self.config.issuer) {
            return Err(Response::error(
                502,
                "issuer_error",
                "Authy issuer mismatch",
            ));
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
    fn endpoint(&self, metadata: &Value, name: &str) -> Result<String, Response> {
        metadata[name]
            .as_str()
            .filter(|s| s.starts_with(&format!("{}/", self.config.issuer)))
            .map(String::from)
            .ok_or_else(|| Response::error(502, "issuer_error", "Invalid Authy endpoint"))
    }
    fn client_headers(&self) -> Vec<(String, String)> {
        let component = |s: &str| query(&[("v", s)]).trim_start_matches("v=").to_string();
        vec![
            (
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            (
                "authorization".into(),
                format!(
                    "Basic {}",
                    STANDARD.encode(format!(
                        "{}:{}",
                        component(&self.config.client_id),
                        component(&self.config.client_secret)
                    ))
                ),
            ),
        ]
    }
    pub async fn login(&self, req: &Request) -> Result<Response, Response> {
        if self.config.client_secret.len() < 32 {
            return Err(Response::error(
                503,
                "configuration",
                "Configure a shared CHATTY_CLIENT_SECRET of at least 32 characters",
            ));
        }
        let metadata = self.discovery().await?;
        let state = self.host.random()?;
        let binding = self.host.random()?;
        let nonce = self.host.random()?;
        let verifier = self.host.random()?;
        let old_session = self
            .cookie
            .read(req.header("cookie"))?
            .map(|s| snap_oidc::digest(&s))
            .unwrap_or_default();
        let redirect = format!("{}/auth/callback", self.config.origin);
        let data = json!({"kind":"login","nonce":nonce,"verifier":verifier,"redirect":redirect,"issuer":self.config.issuer,"old_session":old_session});
        tx(
            &self.store,
            vec![],
            vec![
                S::Delete {
                    table: ATTEMPTS,
                    filter: vec![P::le("expires", req.now as i64)],
                },
                S::Insert {
                    table: ATTEMPTS,
                    row: row(&[
                        ("id", snap_oidc::digest(&state).into()),
                        ("binding", snap_oidc::digest(&binding).into()),
                        ("data", stringify(&data)),
                        ("expires", ((req.now + 300_000) as i64).into()),
                        ("processing", 0i64.into()),
                    ]),
                },
            ],
        )
        .await?;
        let url = format!(
            "{}?{}",
            self.endpoint(&metadata, "authorization_endpoint")?,
            query(&[
                ("client_id", &self.config.client_id),
                ("redirect_uri", &redirect),
                ("response_type", "code"),
                ("scope", "openid profile email"),
                ("state", &state),
                ("nonce", data["nonce"].as_str().unwrap_or("")),
                (
                    "code_challenge",
                    &snap_oidc::digest(data["verifier"].as_str().unwrap_or(""))
                ),
                ("code_challenge_method", "S256")
            ])
        );
        Ok(Response::redirect(&url).with("set-cookie", &self.correlation.encode(Some(&binding))))
    }
    async fn attempt(
        &self,
        req: &Request,
        state: &str,
    ) -> Result<(String, snap_store::Row), Response> {
        let binding = self
            .correlation
            .read(req.header("cookie"))?
            .ok_or_else(|| {
                Response::error(400, "invalid_state", "Login browser binding is missing")
            })?;
        let id = snap_oidc::digest(state);
        let record = read(&self.store, live(ATTEMPTS, &id, self.host.now()))
            .await?
            .ok_or_else(|| {
                Response::error(400, "invalid_state", "Login state expired or already used")
            })?;
        if !snap_oidc::same_secret(&text(&record, "binding")?, &snap_oidc::digest(&binding)) {
            return Err(Response::error(
                400,
                "invalid_state",
                "Login belongs to a different browser",
            ));
        }
        Ok((id, record))
    }
    pub async fn callback(&self, req: &Request) -> Result<Response, Response> {
        let params = req.form()?;
        let state = params
            .get("state")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Response::error(400, "invalid_state", "Missing login state"))?;
        let (attempt, record) = self.attempt(req, state).await?;
        let data = json(&record, "data")?;
        if data["kind"] != "login" {
            return Err(Response::error(400, "invalid_state", "Not a login attempt"));
        }
        let mut q = live(ATTEMPTS, &attempt, self.host.now());
        q.filter.push(P::eq("processing", 0i64));
        tx(
            &self.store,
            vec![guard(q)],
            vec![S::Update {
                table: ATTEMPTS,
                filter: vec![P::eq("id", attempt.clone())],
                changes: row(&[("processing", 1i64.into())]),
            }],
        )
        .await?;
        let result = self.finish_callback(req, &params, &attempt, &data).await;
        if result.is_err() {
            let _ = tx(&self.store, vec![], vec![delete(ATTEMPTS, &attempt)]).await;
        }
        result
    }
    async fn finish_callback(
        &self,
        _req: &Request,
        params: &alloc::collections::BTreeMap<String, String>,
        attempt: &str,
        data: &Value,
    ) -> Result<Response, Response> {
        if params.contains_key("error")
            || params.get("iss").map(String::as_str) != Some(&self.config.issuer)
            || data["issuer"].as_str() != Some(&self.config.issuer)
        {
            return Err(Response::error(
                400,
                "login_failed",
                "Authorization was declined or came from a different issuer",
            ));
        }
        let code = params
            .get("code")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Response::error(400, "login_failed", "Missing authorization code"))?;
        let metadata = self.discovery().await?;
        let tokens = self
            .remote(
                "POST",
                self.endpoint(&metadata, "token_endpoint")?,
                self.client_headers(),
                query(&[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", data["redirect"].as_str().unwrap_or("")),
                    ("code_verifier", data["verifier"].as_str().unwrap_or("")),
                ])
                .into_bytes(),
            )
            .await?;
        let (claims, mut session) = self
            .validate_tokens(
                tokens,
                &metadata,
                Some(data["nonce"].as_str().unwrap_or("")),
                None,
            )
            .await?;
        let access = session["access_token"].as_str().ok_or_else(unavailable)?;
        let profile = self
            .remote(
                "GET",
                self.endpoint(&metadata, "userinfo_endpoint")?,
                vec![("authorization".into(), format!("Bearer {access}"))],
                vec![],
            )
            .await?;
        if profile["sub"] != claims["sub"] {
            return Err(Response::error(
                401,
                "login_failed",
                "UserInfo subject does not match the ID token",
            ));
        }
        let bearer = self.host.random()?;
        let csrf = self.host.random()?;
        let now = self.host.now();
        let subject = claims["sub"].as_str().ok_or_else(unavailable)?;
        let owner = snap_oidc::digest(&format!("{}\n{subject}", self.config.issuer));
        session["profile"] = profile;
        session["subject"] = subject.into();
        session["csrf"] = csrf.into();
        session["nonce"] = data["nonce"].clone();
        session["issuer"] = self.config.issuer.clone().into();
        let mut pending = live(ATTEMPTS, attempt, now);
        pending.filter.push(P::eq("processing", 1i64));
        let mut statements = vec![
            delete(ATTEMPTS, attempt),
            S::Delete {
                table: SESSIONS,
                filter: vec![P::le("expires", now as i64)],
            },
        ];
        if let Some(old) = data["old_session"].as_str().filter(|s| !s.is_empty()) {
            statements.push(delete(SESSIONS, old));
        }
        statements.push(S::Insert {
            table: SESSIONS,
            row: row(&[
                ("id", snap_oidc::digest(&bearer).into()),
                ("owner", owner.into()),
                ("data", stringify(&session)),
                ("expires", ((now + 30 * 24 * 3600 * 1000) as i64).into()),
                ("version", 1i64.into()),
                ("refreshing", 0i64.into()),
            ]),
        });
        tx(&self.store, vec![guard(pending)], statements).await?;
        Ok(Response::redirect("/")
            .with("set-cookie", &self.cookie.encode(Some(&bearer)))
            .with("set-cookie", &self.correlation.encode(None)))
    }
    async fn validate_tokens(
        &self,
        tokens: Value,
        metadata: &Value,
        nonce: Option<&str>,
        previous: Option<&Value>,
    ) -> Result<(Value, Value), Response> {
        let bad = || {
            Response::error(
                401,
                "invalid_token",
                "Authy returned an invalid identity token",
            )
        };
        let access = tokens["access_token"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() < 8192 && s.is_ascii())
            .ok_or_else(bad)?;
        if tokens["token_type"]
            .as_str()
            .is_none_or(|s| !s.eq_ignore_ascii_case("Bearer"))
        {
            return Err(bad());
        }
        let raw = tokens["id_token"].as_str().ok_or_else(bad)?;
        let jwks = self
            .remote("GET", self.endpoint(metadata, "jwks_uri")?, vec![], vec![])
            .await?;
        let claims = self.host.verify(raw.into(), jwks).await?;
        let now = self.host.now() / 1000;
        let subject = claims["sub"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 255 && s.is_ascii())
            .ok_or_else(bad)?;
        let audience = claims["aud"].as_str() == Some(&self.config.client_id)
            || claims["aud"]
                .as_array()
                .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some(&self.config.client_id));
        if claims["iss"].as_str() != Some(&self.config.issuer)
            || !audience
            || claims["exp"].as_u64().is_none_or(|n| n <= now)
            || claims["iat"]
                .as_u64()
                .is_none_or(|n| n > now + 30 || now.saturating_sub(n) > 900)
            || claims
                .get("azp")
                .is_some_and(|v| v.as_str() != Some(&self.config.client_id))
        {
            return Err(bad());
        }
        if let Some(nonce) = nonce
            && claims["nonce"]
                .as_str()
                .is_none_or(|n| !snap_oidc::same_secret(n, nonce))
        {
            return Err(bad());
        }
        if let Some(hash) = claims.get("at_hash")
            && hash
                .as_str()
                .is_none_or(|s| !snap_oidc::same_secret(s, &snap_oidc::token_hash(access)))
        {
            return Err(bad());
        }
        if let Some(previous) = previous
            && (previous["subject"].as_str() != Some(subject)
                || claims.get("nonce").is_some_and(|n| n != &previous["nonce"])
                || claims
                    .get("auth_time")
                    .is_some_and(|n| n != &previous["auth_time"]))
        {
            return Err(bad());
        }
        let ttl = tokens["expires_in"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 86400)
            .ok_or_else(bad)?;
        let refresh = tokens["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(bad)?;
        Ok((
            claims.clone(),
            json!({"access_token":access,"refresh_token":refresh,"id_token":raw,"access_expires":self.host.now()+ttl*1000,"auth_time":claims["auth_time"]}),
        ))
    }
    fn decode(&self, record: &snap_store::Row) -> Result<Actor, Response> {
        let data = json(record, "data")?;
        Ok(Actor {
            id: text(record, "id")?,
            owner: text(record, "owner")?,
            subject: data["subject"].as_str().ok_or_else(unavailable)?.into(),
            csrf: data["csrf"].as_str().ok_or_else(unavailable)?.into(),
            version: number(record, "version")?,
            expires: number(record, "expires")? as u64,
            data,
        })
    }
    pub async fn resolve(&self, req: &Request) -> Result<Option<Actor>, Response> {
        let Some(token) = self.cookie.read(req.header("cookie"))? else {
            return Ok(None);
        };
        let id = snap_oidc::digest(&token);
        for _ in 0..100 {
            let Some(record) = read(&self.store, live(SESSIONS, &id, self.host.now())).await?
            else {
                return Ok(None);
            };
            if number(&record, "refreshing")? != 0 {
                self.host.sleep(20).await;
                continue;
            }
            let actor = self.decode(&record)?;
            if actor.data["access_expires"].as_u64().unwrap_or(0) > self.host.now() + 30_000 {
                return Ok(Some(actor));
            }
            let result = tx(
                &self.store,
                vec![actor.authority(self.host.now())],
                vec![S::Update {
                    table: SESSIONS,
                    filter: vec![P::eq("id", id.clone())],
                    changes: row(&[("refreshing", 1i64.into())]),
                }],
            )
            .await;
            if result.as_ref().is_err_and(|r| r.status == 409) {
                continue;
            }
            result?;
            match self.refresh(actor).await {
                Ok(actor) => return Ok(Some(actor)),
                Err(error) => {
                    let _ = tx(&self.store, vec![], vec![delete(SESSIONS, &id)]).await;
                    return Err(error);
                }
            }
        }
        Err(Response::error(
            503,
            "session_busy",
            "Session refresh is in progress",
        ))
    }
    async fn refresh(&self, mut actor: Actor) -> Result<Actor, Response> {
        let metadata = self.discovery().await?;
        let tokens = self
            .remote(
                "POST",
                self.endpoint(&metadata, "token_endpoint")?,
                self.client_headers(),
                query(&[
                    ("grant_type", "refresh_token"),
                    (
                        "refresh_token",
                        actor.data["refresh_token"]
                            .as_str()
                            .ok_or_else(unavailable)?,
                    ),
                ])
                .into_bytes(),
            )
            .await?;
        let (_, mut data) = self
            .validate_tokens(tokens, &metadata, None, Some(&actor.data))
            .await?;
        for key in ["profile", "subject", "csrf", "nonce", "issuer"] {
            data[key] = actor.data[key].clone();
        }
        let mut q = live(SESSIONS, &actor.id, self.host.now());
        q.filter
            .extend([P::eq("version", actor.version), P::eq("refreshing", 1i64)]);
        actor.version += 1;
        tx(
            &self.store,
            vec![guard(q)],
            vec![S::Update {
                table: SESSIONS,
                filter: vec![P::eq("id", actor.id.clone())],
                changes: row(&[
                    ("data", stringify(&data)),
                    ("version", actor.version.into()),
                    ("refreshing", 0i64.into()),
                ]),
            }],
        )
        .await?;
        actor.data = data;
        Ok(actor)
    }
    pub fn check_csrf(&self, req: &Request, actor: &Actor) -> Result<(), Response> {
        if req.header("origin") != Some(self.config.origin.as_str())
            || req
                .header("x-chatty-csrf")
                .is_none_or(|s| !snap_oidc::same_secret(s, &actor.csrf))
        {
            return Err(Response::error(
                403,
                "csrf",
                "Same-origin session request required",
            ));
        }
        Ok(())
    }
    pub async fn logout(&self, req: &Request) -> Result<Response, Response> {
        let bearer = self
            .cookie
            .read(req.header("cookie"))?
            .ok_or_else(login_required)?;
        let record = read(
            &self.store,
            live(SESSIONS, &snap_oidc::digest(&bearer), self.host.now()),
        )
        .await?
        .ok_or_else(login_required)?;
        let actor = self.decode(&record)?;
        self.check_csrf(req, &actor)?;
        let state = self.host.random()?;
        let binding = self.host.random()?;
        let mut statements = vec![delete(SESSIONS, &actor.id)];
        if let Some(previous) = self.correlation.read(req.header("cookie"))? {
            statements.push(S::Delete {
                table: ATTEMPTS,
                filter: vec![P::eq("binding", snap_oidc::digest(&previous))],
            });
        }
        statements.push(S::Insert {
            table: ATTEMPTS,
            row: row(&[
                ("id", snap_oidc::digest(&state).into()),
                ("binding", snap_oidc::digest(&binding).into()),
                ("data", stringify(&json!({"kind":"logout"}))),
                ("expires", ((self.host.now() + 300_000) as i64).into()),
                ("processing", 0i64.into()),
            ]),
        });
        tx(
            &self.store,
            vec![guard(live(SESSIONS, &actor.id, self.host.now()))],
            statements,
        )
        .await?;
        let redirect = format!(
            "{}/oauth/logout?{}",
            self.config.issuer,
            query(&[
                ("client_id", &self.config.client_id),
                (
                    "post_logout_redirect_uri",
                    &format!("{}/auth/logged-out", self.config.origin)
                ),
                (
                    "id_token_hint",
                    actor.data["id_token"].as_str().unwrap_or("")
                ),
                ("state", &state)
            ])
        );
        Ok(Response::json(200, json!({"redirect":redirect}))
            .with("set-cookie", &self.cookie.encode(None))
            .with("set-cookie", &self.correlation.encode(Some(&binding))))
    }
    pub async fn logged_out(&self, req: &Request) -> Result<Response, Response> {
        let params = req.form()?;
        let state = params
            .get("state")
            .ok_or_else(|| Response::error(400, "invalid_state", "Missing logout state"))?;
        let (id, record) = self.attempt(req, state).await?;
        if json(&record, "data")?["kind"] != "logout" {
            return Err(Response::error(
                400,
                "invalid_state",
                "Not a logout attempt",
            ));
        }
        tx(
            &self.store,
            vec![guard(live(ATTEMPTS, &id, self.host.now()))],
            vec![delete(ATTEMPTS, &id)],
        )
        .await?;
        Ok(Response::redirect("/").with("set-cookie", &self.correlation.encode(None)))
    }
}
