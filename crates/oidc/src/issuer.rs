use crate::{
    Accounts, Crypto, Identity, digest, invalid_grant, same_secret, storage::*, token_hash,
};
use alloc::{borrow::ToOwned, collections::BTreeMap, format, string::String, vec, vec::Vec};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use snap_http::{Request, Response, escape, query};
use snap_store::{Predicate as P, Query, Statement as S, Store};

pub const ROUTES: &[&str] = &[
    "/.well-known/openid-configuration",
    "/oauth/jwks",
    "/oauth/authorize",
    "/oauth/resume",
    "/oauth/token",
    "/oauth/userinfo",
    "/oauth/revoke",
    "/oauth/logout",
];
const ACCESS_SECONDS: u64 = 600;
const GRANT_SECONDS: u64 = 30 * 24 * 3600;
#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub redirect_uri: String,
    pub post_logout_redirect_uri: String,
    /// None explicitly registers a public PKCE client. Confidential clients store
    /// only a digest here and must authenticate with client_secret_basic.
    pub secret_digest: Option<String>,
}
/// Explicitly registered public clients use authorization code + PKCE S256. No
/// implicit/password grant, dynamic registration, wildcard redirects or client
/// secrets are accepted. Every authorization displays consent. Refresh families
/// expire after 30 days and remain subject to the originating login session.
#[derive(Clone)]
pub struct Issuer<S, C, A> {
    pub origin: String,
    pub clients: Vec<Client>,
    pub store: S,
    pub crypto: C,
    pub accounts: A,
}
impl<D: Store, C: Crypto, A: Accounts> Issuer<D, C, A> {
    pub async fn handle(&self, request: Request, actor: Option<Identity>) -> Response {
        match self.dispatch(&request, actor).await {
            Ok(reply) | Err(reply) => reply,
        }
    }
    async fn dispatch(&self, req: &Request, actor: Option<Identity>) -> Result<Response, Response> {
        match req.path.as_str() {
            "/.well-known/openid-configuration" if req.method == "GET" => {
                Ok(Response::json(200, self.discovery()))
            }
            "/oauth/jwks" if req.method == "GET" => Ok(Response::json(200, self.crypto.jwks())),
            "/oauth/authorize" if req.method == "GET" => self.authorize(req, actor).await,
            "/oauth/authorize" if req.method == "POST" => {
                if req.form()?.contains_key("client_id") {
                    self.authorize(req, actor).await
                } else {
                    self.consent(req, actor).await
                }
            }
            "/oauth/resume" if req.method == "GET" => self.resume(req, actor).await,
            "/oauth/token" if req.method == "POST" => self.exchange(req).await,
            "/oauth/revoke" if req.method == "POST" => self.revoke(req).await,
            "/oauth/userinfo" if matches!(req.method.as_str(), "GET" | "POST") => {
                self.userinfo(req).await
            }
            "/oauth/logout" if matches!(req.method.as_str(), "GET" | "POST") => {
                self.logout(req, actor).await
            }
            _ => Err(Response::error(
                405,
                "invalid_request",
                "Method not allowed",
            )),
        }
    }
    fn discovery(&self) -> Value {
        let endpoint = |path: &str| format!("{}{path}", self.origin);
        json!({
            "issuer":self.origin,"authorization_endpoint":endpoint("/oauth/authorize"),
            "token_endpoint":endpoint("/oauth/token"),"userinfo_endpoint":endpoint("/oauth/userinfo"),
            "jwks_uri":endpoint("/oauth/jwks"),"revocation_endpoint":endpoint("/oauth/revoke"),
            "end_session_endpoint":endpoint("/oauth/logout"),"response_types_supported":["code"],
            "response_modes_supported":["query"],"grant_types_supported":["authorization_code","refresh_token"],
            "subject_types_supported":["public"],"id_token_signing_alg_values_supported":["RS256"],
            "token_endpoint_auth_methods_supported":["none","client_secret_basic"],"revocation_endpoint_auth_methods_supported":["none","client_secret_basic"],
            "code_challenge_methods_supported":["S256"],"scopes_supported":["openid","profile","email"],
            "authorization_response_iss_parameter_supported":true,
            "claims_supported":["iss","sub","aud","exp","iat","auth_time","nonce","sid","at_hash","name","email","email_verified","updated_at"],
            "request_parameter_supported":false,"request_uri_parameter_supported":false,"claims_parameter_supported":false
        })
    }
    fn client(&self, id: &str) -> Result<&Client, Response> {
        self.clients
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Response::error(400, "invalid_client", "Unknown client"))
    }
    fn origin(&self, req: &Request) -> Result<(), Response> {
        if req.header("origin") != Some(self.origin.as_str()) {
            return Err(Response::error(
                403,
                "invalid_request",
                "Origin not allowed",
            ));
        }
        Ok(())
    }
    fn authenticate_client(
        &self,
        req: &Request,
        p: &BTreeMap<String, String>,
    ) -> Result<&Client, Response> {
        let bad = || {
            Response::error(401, "invalid_client", "Client authentication failed")
                .with("www-authenticate", "Basic realm=\"Authy\"")
        };
        if p.contains_key("client_secret")
            || req
                .headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .count()
                > 1
        {
            return Err(bad());
        }
        if let Some(header) = req.header("authorization") {
            let encoded = header.strip_prefix("Basic ").ok_or_else(bad)?;
            let bytes = STANDARD.decode(encoded).map_err(|_| bad())?;
            let raw = core::str::from_utf8(&bytes).map_err(|_| bad())?;
            let (id, secret) = raw.split_once(':').ok_or_else(bad)?;
            let decode = |s: &str| -> Result<String, Response> {
                let mut values =
                    snap_http::fields(format!("v={s}").as_bytes()).map_err(|_| bad())?;
                if values.len() != 1 {
                    return Err(bad());
                }
                values.remove("v").ok_or_else(bad)
            };
            let id = decode(id)?;
            let secret = decode(secret)?;
            let client = self.client(&id).map_err(|_| bad())?;
            if p.get("client_id").is_some_and(|v| v != &id)
                || client
                    .secret_digest
                    .as_ref()
                    .is_none_or(|expected| !same_secret(expected, &digest(&secret)))
            {
                return Err(bad());
            }
            Ok(client)
        } else {
            let client = self.client(param(p, "client_id")?).map_err(|_| bad())?;
            if client.secret_digest.is_some() {
                return Err(bad());
            }
            Ok(client)
        }
    }
    async fn authorize(
        &self,
        req: &Request,
        actor: Option<Identity>,
    ) -> Result<Response, Response> {
        let p = req.form()?;
        let client = self.client(param(&p, "client_id")?)?;
        // Never redirect on an unrecognized client/URI, including error paths.
        if param(&p, "redirect_uri")? != client.redirect_uri {
            return Err(bad("Unregistered redirect URI"));
        }
        let state = optional(&p, "state");
        let error = |code: &str, description: &str| {
            redirect_error(&client.redirect_uri, state, code, description, &self.origin)
        };
        if optional(&p, "response_type") != "code" {
            return Err(error(
                "unsupported_response_type",
                "Only authorization code is supported",
            ));
        }
        for (parameter, code) in [
            ("request", "request_not_supported"),
            ("request_uri", "request_uri_not_supported"),
            ("registration", "registration_not_supported"),
        ] {
            if p.contains_key(parameter) {
                return Err(error(code, "Request extension is not supported"));
            }
        }
        if !matches!(optional(&p, "response_mode"), "" | "query") {
            return Err(error(
                "invalid_request",
                "Only query response mode is supported",
            ));
        }
        let scope = optional(&p, "scope");
        if !scope.split_whitespace().any(|s| s == "openid")
            || scope
                .split_whitespace()
                .any(|s| !matches!(s, "openid" | "profile" | "email"))
        {
            return Err(error(
                "invalid_scope",
                "Request openid and supported scopes",
            ));
        }
        let challenge = optional(&p, "code_challenge");
        if optional(&p, "code_challenge_method") != "S256"
            || challenge.len() != 43
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(error("invalid_request", "PKCE S256 is required"));
        }
        if state.len() > 1024 || optional(&p, "nonce").len() > 256 {
            return Err(error("invalid_request", "Parameter too long"));
        }
        let prompts: Vec<_> = optional(&p, "prompt").split_whitespace().collect();
        if prompts
            .iter()
            .any(|p| !matches!(*p, "consent" | "none" | "login" | "select_account"))
            || (prompts.contains(&"none") && prompts.len() != 1)
        {
            return Err(error("invalid_request", "Invalid prompt combination"));
        }
        let max_age = p
            .get("max_age")
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<u32>()
                    .map_err(|_| error("invalid_request", "Invalid max_age"))
            })
            .transpose()?;
        let hint = if let Some(hint) = p.get("id_token_hint").filter(|s| !s.is_empty()) {
            let claims = self
                .crypto
                .verify(hint.clone())
                .await
                .map_err(|_| error("invalid_request", "Invalid ID token hint"))?;
            if claims["iss"].as_str() != Some(&self.origin)
                || claims["aud"].as_str() != Some(&client.id)
            {
                return Err(error("invalid_request", "Invalid ID token hint"));
            }
            Some(
                claims["sub"]
                    .as_str()
                    .ok_or_else(|| error("invalid_request", "Invalid ID token hint"))?
                    .to_owned(),
            )
        } else {
            None
        };
        let reauth = actor.as_ref().is_none_or(|a| {
            max_age.is_some_and(|age| {
                age == 0 || req.now / 1000 > a.auth_time.saturating_add(age as u64)
            }) || hint.as_ref().is_some_and(|s| s != &a.subject)
        }) || prompts.contains(&"login")
            || prompts.contains(&"select_account");
        let mut data = Authorization {
            grant: String::new(),
            client: client.id.clone(),
            redirect: client.redirect_uri.clone(),
            scope: scope.into(),
            state: state.into(),
            nonce: optional(&p, "nonce").into(),
            challenge: challenge.into(),
            subject: hint.unwrap_or_default(),
            session: actor
                .as_ref()
                .map(|a| a.session.clone())
                .unwrap_or_default(),
            auth_time: req.now / 1000,
        };
        if reauth {
            if prompts.contains(&"none") {
                return Err(error("login_required", "Active authentication is required"));
            }
            let handle = self.crypto.random()?;
            transaction(
                &self.store,
                vec![],
                vec![
                    S::Delete {
                        table: FLOWS,
                        filter: vec![P::le("expires", req.now as i64)],
                    },
                    flow(&digest(&handle), "login", &data, req.now + 300_000),
                ],
            )
            .await?;
            return Ok(Response::redirect(&format!(
                "/?{}",
                query(&[
                    (
                        "return_to",
                        &format!("/oauth/resume?{}", query(&[("request", &handle)]))
                    ),
                    ("reauth", if actor.is_some() { "1" } else { "0" })
                ])
            )));
        }
        let actor = actor.expect("reauth covers missing actor");
        if prompts.contains(&"none") {
            return Err(error("consent_required", "Interactive consent is required"));
        }
        data.subject = actor.subject.clone();
        data.session = actor.session.clone();
        data.auth_time = actor.auth_time;
        self.consent_page(data, actor, req.now).await
    }
    async fn resume(&self, req: &Request, actor: Option<Identity>) -> Result<Response, Response> {
        let p = req.form()?;
        let id = digest(param(&p, "request")?);
        let q = live(FLOWS, &id, req.now);
        let row = read(&self.store, q.clone())
            .await?
            .ok_or_else(invalid_grant)?;
        if text(&row, "kind")? != "login" {
            return Err(invalid_grant());
        }
        let mut data = authorization(&row)?;
        let actor = actor.ok_or_else(invalid_grant)?;
        if actor.session == data.session
            || actor.auth_time < data.auth_time
            || (!data.subject.is_empty() && data.subject != actor.subject)
        {
            return Err(Response::error(
                400,
                "login_required",
                "Complete a fresh sign-in with the requested account",
            ));
        }
        let mut guards = actor.authority.clone();
        guards.push(guard(q));
        transaction(&self.store, guards, vec![delete(FLOWS, &id)]).await?;
        data.subject = actor.subject.clone();
        data.session = actor.session.clone();
        data.auth_time = actor.auth_time;
        self.consent_page(data, actor, req.now).await
    }
    async fn consent_page(
        &self,
        data: Authorization,
        actor: Identity,
        now: u64,
    ) -> Result<Response, Response> {
        let client = self.client(&data.client)?;
        let handle = self.crypto.random()?;
        transaction(
            &self.store,
            actor.authority,
            vec![
                S::Delete {
                    table: FLOWS,
                    filter: vec![P::le("expires", now as i64)],
                },
                flow(&digest(&handle), "consent", &data, now + 300_000),
            ],
        )
        .await?;
        Ok(Response::html(page(
            "Connect to your account",
            &format!(
                "<p><strong>{}</strong> wants to sign you in and access your account's <strong>{}</strong>.</p><p>Signed in as {}.</p><form method=post action=/oauth/authorize><input type=hidden name=request value=\"{}\"><button name=decision value=allow>Continue to {}</button><button class=secondary name=decision value=deny>Cancel</button></form>",
                escape(&client.name),
                escape(&data.scope),
                escape(actor.claims["email"].as_str().unwrap_or("your account")),
                escape(&handle),
                escape(&client.name)
            ),
        )).with("content-security-policy", &form_policy(&client.redirect_uri)))
    }
    async fn consent(&self, req: &Request, actor: Option<Identity>) -> Result<Response, Response> {
        self.origin(req)?;
        let p = req.form()?;
        let id = digest(param(&p, "request")?);
        let q = live(FLOWS, &id, req.now);
        let record = read(&self.store, q.clone())
            .await?
            .ok_or_else(invalid_grant)?;
        if text(&record, "kind")? != "consent" {
            return Err(invalid_grant());
        }
        let data = authorization(&record)?;
        let actor = actor.ok_or_else(invalid_grant)?;
        if actor.subject != data.subject || actor.session != data.session {
            return Err(invalid_grant());
        }
        self.client(&data.client)?;
        let mut guards = actor.authority;
        guards.push(guard(q));
        if optional(&p, "decision") != "allow" {
            transaction(&self.store, guards, vec![delete(FLOWS, &id)]).await?;
            return Ok(redirect_error(
                &data.redirect,
                &data.state,
                "access_denied",
                "Authorization declined",
                &self.origin,
            ));
        }
        let code = self.crypto.random()?;
        transaction(
            &self.store,
            guards,
            vec![
                delete(FLOWS, &id),
                flow(&digest(&code), "code", &data, req.now + 60_000),
            ],
        )
        .await?;
        Ok(Response::redirect(&append(
            &data.redirect,
            &[
                ("code", &code),
                ("state", &data.state),
                ("iss", &self.origin),
            ],
        )))
    }
    async fn exchange(&self, req: &Request) -> Result<Response, Response> {
        let p = req.form()?;
        let client = self.authenticate_client(req, &p)?;
        match param(&p, "grant_type")? {
            "authorization_code" => {
                let id = digest(param(&p, "code")?);
                let q = live(FLOWS, &id, req.now);
                let row = read(&self.store, q.clone())
                    .await?
                    .ok_or_else(invalid_grant)?;
                let mut data = authorization(&row)?;
                if text(&row, "kind")? != "code" {
                    if text(&row, "kind")? == "used_code"
                        && data.client == client.id
                        && !data.grant.is_empty()
                    {
                        transaction(&self.store, vec![], vec![deactivate(&data.grant)]).await?;
                    }
                    return Err(invalid_grant());
                }
                let verifier = param(&p, "code_verifier")?;
                if data.client != client.id
                    || optional(&p, "redirect_uri") != data.redirect
                    || !(43..=128).contains(&verifier.len())
                    || !verifier
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
                    || !same_secret(&digest(verifier), &data.challenge)
                {
                    return Err(invalid_grant());
                }
                let actor = self
                    .accounts
                    .load(data.subject.clone(), data.session.clone(), req.now)
                    .await?
                    .ok_or_else(invalid_grant)?;
                let grant = self.crypto.random()?;
                data.grant = grant.clone();
                let expires = req.now + GRANT_SECONDS * 1000;
                let statements = vec![
                    delete(FLOWS, &id),
                    flow(&id, "used_code", &data, expires),
                    S::Insert {
                        table: GRANTS,
                        row: row_values(&grant, &data, expires),
                    },
                ];
                let mut guards = actor.authority.clone();
                let mut unconsumed = q;
                unconsumed.filter.push(P::eq("kind", "code"));
                guards.push(guard(unconsumed));
                let result = self
                    .issue(&data, actor, &grant, expires, guards, statements, req.now)
                    .await;
                if result.as_ref().is_err_and(|r| r.status == 400)
                    && let Some(record) = read(&self.store, live(FLOWS, &id, req.now)).await?
                    && text(&record, "kind")? == "used_code"
                {
                    let consumed = authorization(&record)?;
                    if consumed.client == client.id {
                        transaction(&self.store, vec![], vec![deactivate(&consumed.grant)]).await?;
                    }
                }
                result
            }
            "refresh_token" => {
                let id = digest(param(&p, "refresh_token")?);
                let record = read(&self.store, live(TOKENS, &id, req.now))
                    .await?
                    .ok_or_else(invalid_grant)?;
                if text(&record, "kind")? != "refresh" {
                    return Err(invalid_grant());
                }
                let grant = text(&record, "grant_id")?;
                let q = active_grant(&grant, req.now);
                let record_grant = read(&self.store, q.clone())
                    .await?
                    .ok_or_else(invalid_grant)?;
                let data = authorization(&record_grant)?;
                if data.client != client.id {
                    return Err(invalid_grant());
                }
                if integer(&record, "used")? != 0 {
                    // A consumed token signals family replay. Revoke successors too.
                    transaction(&self.store, vec![], vec![deactivate(&grant)]).await?;
                    return Err(invalid_grant());
                }
                if p.get("scope").is_some_and(|scope| scope != &data.scope) {
                    return Err(Response::error(
                        400,
                        "invalid_scope",
                        "Refresh must retain the granted scope",
                    ));
                }
                let actor = self
                    .accounts
                    .load(data.subject.clone(), data.session.clone(), req.now)
                    .await?
                    .ok_or_else(invalid_grant)?;
                let mut guards = actor.authority.clone();
                guards.push(guard(q));
                let mut token_query = live(TOKENS, &id, req.now);
                token_query.filter.push(P::eq("used", 0i64));
                guards.push(guard(token_query));
                let statements = vec![S::Update {
                    table: TOKENS,
                    filter: vec![P::eq("id", id.clone())],
                    changes: row(&[("used", 1i64.into())]),
                }];
                let result = self
                    .issue(
                        &data,
                        actor,
                        &grant,
                        integer(&record_grant, "expires")? as u64,
                        guards,
                        statements,
                        req.now,
                    )
                    .await;
                if result.as_ref().is_err_and(|r| r.status == 400)
                    && read(&self.store, live(TOKENS, &id, req.now))
                        .await?
                        .is_some_and(|r| integer(&r, "used").ok() == Some(1))
                {
                    transaction(&self.store, vec![], vec![deactivate(&grant)]).await?;
                }
                result
            }
            _ => Err(Response::error(
                400,
                "unsupported_grant_type",
                "Only authorization_code and refresh_token are supported",
            )),
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn issue(
        &self,
        data: &Authorization,
        actor: Identity,
        grant: &str,
        grant_expires: u64,
        guards: Vec<snap_store::Guard>,
        mut statements: Vec<S>,
        now: u64,
    ) -> Result<Response, Response> {
        let access = self.crypto.random()?;
        let mut claims = scoped_claims(&actor, data);
        claims["iss"] = self.origin.clone().into();
        claims["aud"] = data.client.clone().into();
        claims["iat"] = (now / 1000).into();
        claims["exp"] = (now / 1000 + ACCESS_SECONDS).into();
        claims["auth_time"] = data.auth_time.into();
        claims["sid"] = data.session.clone().into();
        claims["at_hash"] = token_hash(&access).into();
        if !data.nonce.is_empty() {
            claims["nonce"] = data.nonce.clone().into();
        }
        let id_token = self.crypto.sign(claims).await?;
        statements.push(token(
            &digest(&access),
            grant,
            "access",
            (now + ACCESS_SECONDS * 1000).min(grant_expires),
        ));
        let mut body = json!({"access_token":access,"token_type":"Bearer","expires_in":ACCESS_SECONDS,"id_token":id_token,"scope":data.scope});
        {
            let refresh = self.crypto.random()?;
            statements.push(token(&digest(&refresh), grant, "refresh", grant_expires));
            body["refresh_token"] = refresh.into();
        }
        statements.push(S::Delete {
            table: TOKENS,
            filter: vec![P::le("expires", now as i64)],
        });
        statements.push(S::Delete {
            table: GRANTS,
            filter: vec![P::le("expires", now as i64)],
        });
        transaction(&self.store, guards, statements).await?;
        Ok(Response::json(200, body))
    }
    async fn userinfo(&self, req: &Request) -> Result<Response, Response> {
        let denied = || {
            Response::error(401, "invalid_token", "Access token expired or invalid")
                .with("www-authenticate", "Bearer error=\"invalid_token\"")
        };
        let bearer = req
            .header("authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .filter(|s| !s.is_empty())
            .ok_or_else(denied)?;
        let token_id = digest(bearer);
        let tq = live(TOKENS, &token_id, req.now);
        let record = read(&self.store, tq.clone()).await?.ok_or_else(denied)?;
        if text(&record, "kind")? != "access" {
            return Err(denied());
        }
        let grant = text(&record, "grant_id")?;
        let gq = active_grant(&grant, req.now);
        let record = read(&self.store, gq.clone()).await?.ok_or_else(denied)?;
        let data = authorization(&record)?;
        let actor = self
            .accounts
            .load(data.subject.clone(), data.session.clone(), req.now)
            .await?
            .ok_or_else(denied)?;
        let mut guards = actor.authority.clone();
        guards.extend([guard(tq), guard(gq)]);
        transaction(&self.store, guards, vec![])
            .await
            .map_err(|_| denied())?;
        Ok(Response::json(200, scoped_claims(&actor, &data)))
    }
    async fn revoke(&self, req: &Request) -> Result<Response, Response> {
        let p = req.form()?;
        let client = self.authenticate_client(req, &p)?;
        let id = digest(param(&p, "token")?);
        if let Some(record) = read(&self.store, live(TOKENS, &id, req.now)).await? {
            let grant = text(&record, "grant_id")?;
            if let Some(record) = read(&self.store, live(GRANTS, &grant, req.now)).await?
                && authorization(&record)?.client == client.id
            {
                transaction(&self.store, vec![], vec![deactivate(&grant)]).await?;
            }
        }
        Ok(Response::new(200, "application/json", Vec::new()))
    }
    async fn logout(&self, req: &Request, actor: Option<Identity>) -> Result<Response, Response> {
        let p = req.form()?;
        if req.method == "GET" || !p.contains_key("request") {
            // A signature-verified ID token binds the redirect to the relying party.
            // Expired ID tokens remain valid logout hints, never authentication.
            let hint = if let Some(token) = p.get("id_token_hint").filter(|s| !s.is_empty()) {
                let hint = self.crypto.verify(token.clone()).await?;
                if hint["iss"].as_str() != Some(&self.origin)
                    || hint["sub"].as_str().is_none()
                    || hint["sid"].as_str().is_none()
                {
                    return Err(bad("Invalid logout hint"));
                }
                Some(hint)
            } else {
                None
            };
            let client_id = p
                .get("client_id")
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .or_else(|| hint.as_ref().and_then(|h| h["aud"].as_str()));
            let client = client_id.map(|id| self.client(id)).transpose()?;
            if let Some(hint) = &hint
                && client.is_none_or(|c| hint["aud"].as_str() != Some(&c.id))
            {
                return Err(bad("Invalid logout client"));
            }
            let redirect = p
                .get("post_logout_redirect_uri")
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .unwrap_or("/");
            if redirect != "/" && client.is_none_or(|c| c.post_logout_redirect_uri != redirect) {
                return Err(bad("Unregistered logout redirect"));
            }
            let Some(actor) = actor else {
                return Ok(Response::redirect(&append(
                    redirect,
                    &[("state", optional(&p, "state"))],
                )));
            };
            // Always confirm, including absent hints and hints for another login.
            // This avoids allowing a cross-site GET to end the browser's session.
            let handle = self.crypto.random()?;
            let data = Authorization {
                grant: String::new(),
                client: client.map(|c| c.id.clone()).unwrap_or_default(),
                redirect: redirect.into(),
                scope: String::new(),
                state: optional(&p, "state").into(),
                nonce: String::new(),
                challenge: String::new(),
                subject: actor.subject,
                session: actor.session,
                auth_time: actor.auth_time,
            };
            transaction(
                &self.store,
                actor.authority,
                vec![flow(&digest(&handle), "logout", &data, req.now + 300_000)],
            )
            .await?;
            return Ok(Response::html(page(
                "Sign out of Authy?",
                &format!(
                    "<p>Sign out {}? This ends this Authy login and the grants issued from it.</p><form method=post action=/oauth/logout><input type=hidden name=request value=\"{}\"><button>Sign out</button></form>",
                    escape(actor.claims["email"].as_str().unwrap_or("this account")),
                    escape(&handle)
                ),
            )).with("content-security-policy", &form_policy(redirect)));
        }
        self.origin(req)?;
        let id = digest(param(&p, "request")?);
        let q = live(FLOWS, &id, req.now);
        let record = read(&self.store, q.clone())
            .await?
            .ok_or_else(invalid_grant)?;
        if text(&record, "kind")? != "logout" {
            return Err(invalid_grant());
        }
        let data = authorization(&record)?;
        let actor = actor.ok_or_else(invalid_grant)?;
        if actor.subject != data.subject || actor.session != data.session {
            return Err(invalid_grant());
        }
        let mut guards = actor.authority;
        guards.push(guard(q));
        transaction(&self.store, guards, vec![delete(FLOWS, &id)]).await?;
        self.accounts
            .end_session(data.subject, data.session.clone(), req.now)
            .await?;
        let mut response = Response::redirect(&append(&data.redirect, &[("state", &data.state)]));
        response.terminate.push(data.session);
        Ok(response)
    }
}
fn scoped_claims(actor: &Identity, data: &Authorization) -> Value {
    let mut value = json!({"sub":actor.subject});
    for scope in data.scope.split_whitespace() {
        let names: &[&str] = match scope {
            "profile" => &["name", "updated_at"],
            "email" => &["email", "email_verified"],
            _ => &[],
        };
        for key in names {
            if let Some(claim) = actor.claims.get(key) {
                value[*key] = claim.clone();
            }
        }
    }
    value
}
fn row_values(id: &str, data: &Authorization, expires: u64) -> snap_store::Row {
    row(&[
        ("id", id.into()),
        (
            "data",
            serde_json::to_string(data)
                .expect("authorization serialization")
                .into(),
        ),
        ("active", 1i64.into()),
        ("expires", (expires as i64).into()),
    ])
}
fn active_grant(id: &str, now: u64) -> Query {
    let mut q = live(GRANTS, id, now);
    q.filter.push(P::eq("active", 1i64));
    q
}
fn deactivate(id: &str) -> S {
    S::Update {
        table: GRANTS,
        filter: vec![P::eq("id", id)],
        changes: row(&[("active", 0i64.into())]),
    }
}
fn bad(description: &str) -> Response {
    Response::error(400, "invalid_request", description)
}
fn param<'a>(p: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, Response> {
    p.get(key)
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .ok_or_else(|| bad(&format!("Missing {key}")))
}
fn optional<'a>(p: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    p.get(key).map(String::as_str).unwrap_or("")
}
fn append(uri: &str, params: &[(&str, &str)]) -> String {
    format!(
        "{uri}{}{}",
        if uri.contains('?') { "&" } else { "?" },
        query(params)
    )
}
fn redirect_error(uri: &str, state: &str, code: &str, description: &str, issuer: &str) -> Response {
    Response::redirect(&append(
        uri,
        &[
            ("error", code),
            ("error_description", description),
            ("state", state),
            ("iss", issuer),
        ],
    ))
}
fn page(title: &str, content: &str) -> String {
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>{title} · Authy</title><style>body{{background:#f5f3ee;color:#202923;font:17px system-ui;margin:0;padding:8vh 24px}}main{{max-width:520px;margin:auto;background:white;border:1px solid #deded5;border-radius:20px;padding:36px}}small{{color:#687668}}h1{{font-size:30px}}p{{line-height:1.6}}button{{border:0;background:#234d3c;color:white;border-radius:8px;padding:13px 20px;font:inherit;cursor:pointer;margin:12px 8px 0 0}}.secondary{{background:#eeeee8;color:#222}}</style><main><small>AUTHY / ACCOUNT ACCESS</small><h1>{title}</h1>{content}</main></html>"
    )
}
/// The destination comes from validated registration. Browsers enforce form-action
/// on redirects too, so self alone would block an ordinary cross-origin RP callback.
fn form_policy(redirect: &str) -> String {
    let destination = if redirect.starts_with('/') {
        "'self'"
    } else {
        redirect
    };
    format!(
        "default-src 'none'; style-src 'unsafe-inline'; form-action 'self' {destination}; frame-ancestors 'none'; base-uri 'none'"
    )
}
