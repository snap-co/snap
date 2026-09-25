use crate::account::Accounts;
use alloc::boxed::Box;
use snap_http::{Cookie, FutureValue, Request, Response, Service};
use snap_runtime::passport::Crypto;
use snap_store::{Cache, Store};

pub const ROUTES: &[&str] = &[
    "/api/account",
    "/.well-known/openid-configuration",
    "/oauth/jwks",
    "/oauth/authorize",
    "/oauth/resume",
    "/oauth/token",
    "/oauth/userinfo",
    "/oauth/revoke",
    "/oauth/logout",
];
#[derive(Clone)]
pub struct Web<S, C, K, O, H> {
    pub issuer: snap_oidc::Issuer<S, O, Accounts<S, C, K>>,
    pub cookie: H,
}
impl<S: Store, C: Crypto, K: Cache, O: snap_oidc::Crypto, H: Cookie> Service
    for Web<S, C, K, O, H>
{
    fn routes(&self) -> &'static [&'static str] {
        ROUTES
    }
    fn call(&self, req: Request) -> FutureValue<Response> {
        let web = self.clone();
        Box::pin(async move {
            match web.handle(req).await {
                Ok(r) | Err(r) => r,
            }
        })
    }
}
impl<S: Store, C: Crypto, K: Cache, O: snap_oidc::Crypto, H: Cookie> Web<S, C, K, O, H> {
    async fn handle(&self, req: Request) -> Result<Response, Response> {
        let needs_cookie = matches!(
            req.path.as_str(),
            "/api/account" | "/oauth/authorize" | "/oauth/resume" | "/oauth/logout"
        );
        let actor = if needs_cookie {
            match self.cookie.read(req.header("cookie"))? {
                Some(token) => self.issuer.accounts.resolve(&token, req.now).await?,
                None => None,
            }
        } else {
            None
        };
        if req.path == "/api/account" {
            let actor =
                actor.ok_or_else(|| Response::error(401, "login_required", "Sign in at Authy"))?;
            let value = match req.method.as_str() {
                "GET" => self.issuer.accounts.profile(actor).await?,
                "POST" => {
                    if req.header("origin") != Some(self.issuer.origin.as_str())
                        || req
                            .header("content-type")
                            .is_none_or(|h| h.split(';').next() != Some("application/json"))
                    {
                        return Err(Response::error(
                            403,
                            "invalid_request",
                            "Same-origin JSON request required",
                        ));
                    }
                    let value = serde_json::from_slice(&req.body)
                        .map_err(|_| Response::error(400, "invalid_request", "Invalid JSON"))?;
                    self.issuer.accounts.update(actor, value, req.now).await?
                }
                _ => {
                    return Err(Response::error(
                        405,
                        "invalid_request",
                        "Method not allowed",
                    ));
                }
            };
            return Ok(Response::json(200, value));
        }
        let logout = req.path == "/oauth/logout" && req.method == "POST";
        let mut response = self.issuer.handle(req, actor).await;
        if logout && response.status == 303 {
            response = response.with("set-cookie", &self.cookie.encode(None));
        }
        Ok(response)
    }
}
