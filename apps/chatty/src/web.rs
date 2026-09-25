use crate::{Config, Host, session::Sessions, threads::Threads};
use alloc::{boxed::Box, string::String};
use serde_json::{Value, json};
use snap_http::{Cookie, FutureValue, Request, Response, Service};
use snap_store::Store;
pub const ROUTES: &[&str] = &[
    "/auth/login",
    "/auth/callback",
    "/auth/logout",
    "/auth/logged-out",
    "/api/session",
    "/api/threads",
    "/api/thread",
    "/api/thread/create",
    "/api/thread/rename",
    "/api/thread/delete",
    "/api/send",
    "/api/cancel",
];
#[derive(Clone)]
pub struct Web<S, H, K> {
    pub sessions: Sessions<S, H, K>,
    pub threads: Threads<S, H>,
}
impl<S: Store, H: Host, K: Cookie> Web<S, H, K> {
    pub fn new(store: S, host: H, config: Config, cookie: K, correlation: K) -> Self {
        Self {
            sessions: Sessions {
                store: store.clone(),
                host: host.clone(),
                config: config.clone(),
                cookie,
                correlation,
            },
            threads: Threads::new(store, host, config),
        }
    }
    async fn dispatch(&self, req: &Request) -> Result<Response, Response> {
        match (req.path.as_str(), req.method.as_str()) {
            ("/auth/login", "GET") => return self.sessions.login(req).await,
            ("/auth/callback", "GET") => return self.sessions.callback(req).await,
            ("/auth/logged-out", "GET") => return self.sessions.logged_out(req).await,
            ("/auth/logout", "POST") => return self.sessions.logout(req).await,
            _ => {}
        }
        let actor = self.sessions.resolve(req).await?;
        if req.path == "/api/session" && req.method == "GET" {
            let mut value = actor
                .map(|a| a.public())
                .unwrap_or_else(|| json!({"identified":false}));
            value["model"] = self.sessions.config.model.model.clone().into();
            value["model_ready"] = (!self.sessions.config.model.key.is_empty()).into();
            value["files_available"] = self.sessions.config.files.into();
            value["search_available"] = (!self.sessions.config.exa_key.is_empty()).into();
            return Ok(Response::json(200, value));
        }
        let actor = actor.ok_or_else(crate::storage::login_required)?;
        let input = if req.method == "POST" {
            self.sessions.check_csrf(req, &actor)?;
            if req
                .header("content-type")
                .is_none_or(|s| s.split(';').next() != Some("application/json"))
            {
                return Err(Response::error(415, "invalid_request", "Expected JSON"));
            }
            serde_json::from_slice::<Value>(&req.body)
                .map_err(|_| Response::error(400, "invalid_request", "Invalid JSON"))?
        } else {
            Value::Null
        };
        let output = match (req.path.as_str(), req.method.as_str()) {
            ("/api/threads", "GET") => self.threads.list(&actor).await?,
            ("/api/thread", "GET") => {
                let params = req.form()?;
                self.threads
                    .view(&actor, params.get("id").map(String::as_str).unwrap_or(""))
                    .await?
            }
            ("/api/thread/create", "POST") => self.threads.create(&actor, &input).await?,
            ("/api/thread/rename", "POST") => self.threads.rename(&actor, &input).await?,
            ("/api/thread/delete", "POST") => {
                self.threads
                    .delete(&actor, crate::threads::field(&input, "thread_id")?)
                    .await?
            }
            ("/api/send", "POST") => self.threads.send(actor, &input).await?,
            ("/api/cancel", "POST") => self.threads.cancel(&actor, &input).await?,
            _ => {
                return Err(Response::error(
                    405,
                    "invalid_request",
                    "Method not allowed",
                ));
            }
        };
        Ok(Response::json(
            if req.path == "/api/send" { 202 } else { 200 },
            output,
        ))
    }
}
impl<S: Store, H: Host, K: Cookie> Service for Web<S, H, K> {
    fn routes(&self) -> &'static [&'static str] {
        ROUTES
    }
    fn call(&self, req: Request) -> FutureValue<Response> {
        let web = self.clone();
        Box::pin(async move {
            match web.dispatch(&req).await {
                Ok(r) | Err(r) => r,
            }
        })
    }
}
