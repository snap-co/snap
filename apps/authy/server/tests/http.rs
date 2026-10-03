mod support;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use support::{CHALLENGE, CLIENT_SECRET, Host, VERIFIER, cookie, destination, query, value};

#[tokio::test]
#[ignore = "real HTTP/WebSocket, storage and password hashing gate; prepare Authy web assets"]
async fn account_operations_preserve_credential_cookie_and_session_authority() {
    let mut host = Host::new("http://127.0.0.1:3850", None).await;
    let body = json!({"email":"  Account@Example.test ","password":"account fixture password"});
    for path in ["/api/signup", "/api/login", "/api/session"] {
        assert_eq!(
            host.post(path, &body, "", &host.base)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(host.request(path, "").send().await.unwrap().status(), 404);
    }
    async fn upgrade(host: &Host, cookie: &str) -> u16 {
        host.request("/transport", cookie)
            .header("origin", &host.base)
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
    assert_eq!(upgrade(&host, "").await, 401);
    assert_eq!(
        host.post("/identity/enroll", &body, "", "https://other.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        host.post(
            "/identity/enroll",
            &json!({"email":"Account@Example.test","password":"short"}),
            "",
            &host.base
        )
        .send()
        .await
        .unwrap()
        .status(),
        400
    );
    let created = host
        .post("/identity/enroll", &body, "", &host.base)
        .header("x-snap-operation-id", "77")
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200);
    let first = cookie(&created);
    let completion = value(created).await;
    let account = value(host.request("/authy/account", &first).send().await.unwrap()).await["Completed"]["outcome"]["Ok"].clone();
    assert_eq!(
        completion["Completed"]["outcome"]["Ok"]["identity"],
        account["identity"]
    );
    assert!(
        completion["Completed"]["outcome"]["Ok"]
            .get("session")
            .is_none()
    );
    assert!(
        completion["Completed"]["outcome"]["Ok"]
            .get("bearer")
            .is_none()
    );
    assert_eq!(completion["Completed"]["id"], 77);
    assert_eq!(account["email"], "account@example.test");
    assert_eq!(
        host.post("/identity/enroll", &body, "", &host.base)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let invalid = host
        .post(
            "/identity/acquire",
            &json!({"email":"Account@Example.test","password":"incorrect password"}),
            "",
            &host.base,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 401);
    assert!(invalid.headers().get("set-cookie").is_none());
    let logged_in = host
        .post("/identity/acquire", &body, "", &host.base)
        .send()
        .await
        .unwrap();
    assert_eq!(logged_in.status(), 200);
    let second = cookie(&logged_in);
    assert_eq!(
        value(logged_in).await["Completed"]["outcome"]["Ok"]["identity"],
        account["identity"]
    );
    assert!(
        host.invoke(&first, "identity.acquire", body.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("UnknownOperation")
    );
    assert_ne!(
        host.post("/authy/logout", &json!({"scope":"all"}), &first, &host.base)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let sessions = value(
        host.request("/identity/sessions", &first)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let sessions = sessions["Completed"]["outcome"]["Ok"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session["current"] == true)
            .count(),
        1
    );
    assert_eq!(
        value(
            host.request("/identity/credentials", &first)
                .send()
                .await
                .unwrap()
        )
        .await["Completed"]["outcome"]["Ok"],
        json!([{"locator":"account@example.test","label":"account@example.test","kind":"password","removable":false}])
    );
    assert_ne!(
        host.post("/api/logout", &json!({"scope":"all"}), &first, &host.base)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        host.post(
            "/identity/release",
            &json!({"scope":"others"}),
            &first,
            &host.base
        )
        .send()
        .await
        .unwrap()
        .status(),
        200
    );
    let expired = host
        .request("/identity/fetch", &second)
        .send()
        .await
        .unwrap();
    assert!(
        expired.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert!(value(expired).await["Completed"]["outcome"]["Ok"].is_null());
    assert_eq!(upgrade(&host, &second).await, 401);
    host.restart().await;
    assert_eq!(
        value(
            host.request("/identity/fetch", &first)
                .send()
                .await
                .unwrap()
        )
        .await["Completed"]["outcome"]["Ok"]["identity"],
        account["identity"]
    );
    let released = host
        .post(
            "/identity/release",
            &json!({"scope":"current"}),
            &first,
            &host.base,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(released.status(), 200);
    assert!(
        released.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert!(
        value(
            host.request("/identity/fetch", &first)
                .send()
                .await
                .unwrap()
        )
        .await["Completed"]["outcome"]["Ok"]
            .is_null()
    );
}

#[tokio::test]
#[ignore = "real HTTP, cookie, storage and password hashing gate; prepare Authy web assets"]
async fn canonical_origins_preserve_password_issuer_and_cookie_behavior_after_restart() {
    // Authy owns host compatibility: optional passkey eligibility must not decide
    // whether an otherwise valid password/issuer development host can start.
    for (configured, origin, secure) in [
        ("HTTPS://AUTHY.EXAMPLE:443", "https://authy.example", true),
        (
            "http://authy.example.test:3846",
            "http://authy.example.test:3846",
            false,
        ),
    ] {
        let mut host = Host::new("http://127.0.0.1:3850", Some(configured)).await;
        let discovery = value(
            host.request("/.well-known/openid-configuration", "")
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(discovery["issuer"], origin);
        let created = host
            .post(
                "/identity/enroll",
                &json!({"email":"secure@example.test","password":"secure fixture password"}),
                "",
                origin,
            )
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 200);
        let header = created.headers()["set-cookie"].to_str().unwrap();
        assert!(header.starts_with(if secure {
            "__Host-authy_session="
        } else {
            "authy_session="
        }));
        assert_eq!(header.contains("; Secure"), secure);
        assert!(header.contains("; HttpOnly; SameSite=Lax;"));
        let cookie = cookie(&created);
        if !secure {
            // An unmounted operation reaches the read-only frontend fallback,
            // which rejects POST rather than accepting a passkey ceremony.
            assert_eq!(
                host.post(
                    "/identity/passkey-authenticate",
                    &json!({"locator":null,"binding":"binding-with-at-least-32-characters"}),
                    "",
                    origin
                )
                .send()
                .await
                .unwrap()
                .status(),
                405
            );
        }
        host.restart().await;
        assert_eq!(
            value(
                host.request("/authy/account", &cookie)
                    .send()
                    .await
                    .unwrap()
            )
            .await["Completed"]["outcome"]["Ok"]["email"],
            "secure@example.test"
        );
        let acquired = host
            .post(
                "/identity/acquire",
                &json!({"email":"secure@example.test","password":"secure fixture password"}),
                "",
                origin,
            )
            .send()
            .await;
        let acquired = acquired.unwrap();
        assert_eq!(acquired.status(), 200);
        assert_eq!(
            value(
                host.request("/authy/account", &support::cookie(&acquired))
                    .send()
                    .await
                    .unwrap()
            )
            .await["Completed"]["outcome"]["Ok"]["email"],
            "secure@example.test"
        );
    }
}

fn authorize_url(host: &Host, state: &str, extra: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(&format!("{}/oauth/authorize", host.base)).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", "chatty"),
        (
            "redirect_uri",
            &format!("{}/auth/callback", host.relying_party),
        ),
        ("response_type", "code"),
        ("scope", "openid profile email"),
        ("state", state),
        ("nonce", "fixture-nonce"),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
    ]);
    for (key, val) in extra {
        let retained: Vec<_> = url
            .query_pairs()
            .filter(|(name, _)| name != key)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(retained)
            .append_pair(key, val);
    }
    url.into()
}

async fn exchange(host: &Host, code: &str, verifier: &str) -> reqwest::Response {
    host.form(
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            (
                "redirect_uri",
                &format!("{}/auth/callback", host.relying_party),
            ),
            ("code_verifier", verifier),
        ],
        "",
    )
    .basic_auth("chatty", Some(CLIENT_SECRET))
    .send()
    .await
    .unwrap()
}
async fn refresh(host: &Host, token: &str) -> reqwest::Response {
    host.form(
        "/oauth/token",
        &[("grant_type", "refresh_token"), ("refresh_token", token)],
        "",
    )
    .basic_auth("chatty", Some(CLIENT_SECRET))
    .send()
    .await
    .unwrap()
}
async fn info(host: &Host, token: &str) -> u16 {
    host.request("/oauth/userinfo", "")
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}
fn claim(token: &str) -> Value {
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(token.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap()
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}
fn ticket(html: &str) -> &str {
    html.split("name=\"request\"")
        .nth(1)
        .or_else(|| html.split("name=request").nth(1))
        .unwrap()
        .split("value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
}
async fn authorize(host: &Host, cookie: &str, state: &str) -> String {
    let response = host
        .client
        .get(authorize_url(host, state, &[]))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    let html = response.text().await.unwrap();
    let values = [("request", ticket(&html)), ("decision", "allow")];
    let response = host
        .form("/oauth/authorize", &values, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    let url = destination(&response);
    assert_eq!(url.origin().ascii_serialization(), host.relying_party);
    assert_eq!(query(&url, "state"), state);
    assert_eq!(query(&url, "iss"), host.base);
    assert_eq!(
        host.form("/oauth/authorize", &values, cookie)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    query(&url, "code")
}

#[tokio::test]
#[ignore = "real OIDC HTTP, storage and crypto gate; prepare Authy web assets"]
async fn snapco_login_returns_redeemable_code_without_consent() {
    let host = Host::new("https://chatty.snapco.dev", None).await;
    let start = host
        .client
        .get(authorize_url(&host, "snapco-login", &[]))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 303);
    let url = url::Url::parse(&host.base)
        .unwrap()
        .join(start.headers()["location"].to_str().unwrap())
        .unwrap();
    let resume = query(&url, "continue");
    assert!(resume.starts_with("/oauth/resume?"));
    let (cookie, _) = host
        .login(true, "snapco@example.test", "snapco fixture password")
        .await;
    for url in [
        format!("{}{resume}", host.base),
        authorize_url(&host, "snapco-login", &[("prompt", "none")]),
    ] {
        let response = host
            .client
            .get(url)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 303);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let destination = destination(&response);
        assert_eq!(
            format!(
                "{}{}",
                destination.origin().ascii_serialization(),
                destination.path()
            ),
            format!("{}/auth/callback", host.relying_party)
        );
        assert_eq!(query(&destination, "state"), "snapco-login");
        assert_eq!(query(&destination, "iss"), host.base);
        let token = exchange(&host, &query(&destination, "code"), VERIFIER).await;
        assert_eq!(token.status(), 200);
        assert!(value(token).await["access_token"].is_string());
    }
}

#[tokio::test]
#[ignore = "real OIDC HTTP, restart, storage and crypto gate; prepare Authy web assets"]
async fn issuer_consent_pkce_signed_claims_restart_refresh_replay_revocation_and_logout() {
    use rsa::{
        BigUint, RsaPublicKey,
        pkcs1v15::{Signature, VerifyingKey},
        signature::Verifier,
    };
    use sha2::{Digest, Sha256};
    let mut host = Host::new("http://127.0.0.1:3850", None).await;
    let state = "fixture &+suffix=value%# fragment λ";
    let discovery = value(
        host.request("/.well-known/openid-configuration", "")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(discovery["issuer"], host.base);
    assert_eq!(
        discovery["code_challenge_methods_supported"],
        json!(["S256"])
    );
    let jwks = value(host.request("/oauth/jwks", "").send().await.unwrap()).await;
    assert_eq!(jwks["keys"].as_array().unwrap().len(), 1);
    assert!(jwks["keys"][0].get("d").is_none());
    let bad = host
        .client
        .get(authorize_url(
            &host,
            state,
            &[("redirect_uri", "https://attacker.invalid/")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert!(bad.headers().get("location").is_none());
    let silent = host
        .client
        .get(authorize_url(&host, state, &[("prompt", "none")]))
        .send()
        .await
        .unwrap();
    assert_eq!(query(&destination(&silent), "error"), "login_required");
    assert_eq!(query(&destination(&silent), "state"), state);
    let (mut cookie, account) = host
        .login(true, "oidc@example.test", "OIDC fixture password")
        .await;
    // Keep the bearer and base64 encoding valid: rejection must exercise the
    // signature check rather than malformed padding in the final character.
    let (bearer, signature) = cookie.rsplit_once('.').unwrap();
    let mut signature = URL_SAFE_NO_PAD.decode(signature).unwrap();
    signature[0] ^= 1;
    let tampered = format!("{bearer}.{}", URL_SAFE_NO_PAD.encode(signature));
    for cookie in [tampered, format!("{cookie}; {cookie}")] {
        assert!(
            value(
                host.request("/identity/fetch", &cookie)
                    .send()
                    .await
                    .unwrap()
            )
            .await["Completed"]["outcome"]["Ok"]
                .is_null()
        );
    }
    let code = authorize(&host, &cookie, state).await;
    assert_eq!(exchange(&host, &code, &"x".repeat(43)).await.status(), 400);
    let unauthenticated = host
        .form(
            "/oauth/token",
            &[
                ("client_id", "chatty"),
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", "http://127.0.0.1:3850/auth/callback"),
                ("code_verifier", VERIFIER),
            ],
            "",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);
    let response = exchange(&host, &code, VERIFIER).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let tokens = value(response).await;
    let jwt: Vec<_> = text(&tokens, "id_token").split('.').collect();
    let key = &jwks["keys"][0];
    let key = RsaPublicKey::new(
        BigUint::from_bytes_be(&URL_SAFE_NO_PAD.decode(text(key, "n")).unwrap()),
        BigUint::from_bytes_be(&URL_SAFE_NO_PAD.decode(text(key, "e")).unwrap()),
    )
    .unwrap();
    VerifyingKey::<Sha256>::new(key)
        .verify(
            format!("{}.{}", jwt[0], jwt[1]).as_bytes(),
            &Signature::try_from(URL_SAFE_NO_PAD.decode(jwt[2]).unwrap().as_slice()).unwrap(),
        )
        .unwrap();
    let claims = claim(text(&tokens, "id_token"));
    for (key, expected) in [
        ("iss", json!(host.base)),
        ("aud", json!("chatty")),
        ("sub", account["identity"].clone()),
        ("nonce", json!("fixture-nonce")),
        ("email", json!("oidc@example.test")),
        ("email_verified", json!(false)),
    ] {
        assert_eq!(claims[key], expected);
    }
    assert_eq!(
        claims["at_hash"],
        URL_SAFE_NO_PAD.encode(&Sha256::digest(text(&tokens, "access_token"))[..16])
    );
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        600
    );
    let private = cookie.split('=').nth(1).unwrap().split('.').next().unwrap();
    assert_ne!(
        claims["sid"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(private))
    );
    assert_eq!(info(&host, text(&tokens, "access_token")).await, 200);
    host.restart().await;
    assert_eq!(
        value(host.request("/oauth/jwks", "").send().await.unwrap()).await,
        jwks
    );
    assert_eq!(info(&host, text(&tokens, "access_token")).await, 200);
    assert_eq!(
        value(
            host.request("/identity/fetch", &cookie)
                .send()
                .await
                .unwrap()
        )
        .await["Completed"]["outcome"]["Ok"]["identity"],
        account["identity"]
    );
    let rotated = refresh(&host, text(&tokens, "refresh_token")).await;
    assert_eq!(rotated.status(), 200);
    let rotated = value(rotated).await;
    assert_ne!(rotated["refresh_token"], tokens["refresh_token"]);
    assert_eq!(claim(text(&rotated, "id_token"))["sid"], claims["sid"]);
    assert_eq!(
        refresh(&host, text(&tokens, "refresh_token"))
            .await
            .status(),
        400
    );
    assert_eq!(info(&host, text(&rotated, "access_token")).await, 401);
    assert_eq!(
        refresh(&host, text(&rotated, "refresh_token"))
            .await
            .status(),
        400
    );
    let replay_code = authorize(&host, &cookie, state).await;
    let replay = value(exchange(&host, &replay_code, VERIFIER).await).await;
    assert_eq!(exchange(&host, &replay_code, VERIFIER).await.status(), 400);
    assert_eq!(info(&host, text(&replay, "access_token")).await, 401);
    let revoked =
        value(exchange(&host, &authorize(&host, &cookie, state).await, VERIFIER).await).await;
    assert_eq!(
        host.form(
            "/oauth/revoke",
            &[("token", text(&revoked, "refresh_token"))],
            ""
        )
        .basic_auth("chatty", Some(CLIENT_SECRET))
        .send()
        .await
        .unwrap()
        .status(),
        200
    );
    assert_eq!(info(&host, text(&revoked, "access_token")).await, 401);
    let forced = host
        .client
        .get(authorize_url(&host, state, &[("max_age", "0")]))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), 303);
    let url = url::Url::parse(&host.base)
        .unwrap()
        .join(forced.headers()["location"].to_str().unwrap())
        .unwrap();
    let resume = query(&url, "continue");
    assert!(resume.starts_with("/oauth/resume?"));
    assert_eq!(
        host.request(&resume, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    cookie = host
        .login(false, "oidc@example.test", "OIDC fixture password")
        .await
        .0;
    assert_eq!(
        host.request(&resume, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let active =
        value(exchange(&host, &authorize(&host, &cookie, state).await, VERIFIER).await).await;
    let response = host
        .form(
            "/oauth/logout",
            &[
                ("client_id", "chatty"),
                (
                    "post_logout_redirect_uri",
                    "http://127.0.0.1:3850/auth/logged-out",
                ),
                ("id_token_hint", text(&active, "id_token")),
                ("state", state),
            ],
            &cookie,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let html = response.text().await.unwrap();
    let values = [("request", ticket(&html))];
    let mut forged = host
        .form("/oauth/logout", &values, &cookie)
        .build()
        .unwrap();
    forged
        .headers_mut()
        .insert("origin", "https://attacker.invalid".parse().unwrap());
    assert_eq!(host.client.execute(forged).await.unwrap().status(), 403);
    let ended = host
        .form("/oauth/logout", &values, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(ended.status(), 303);
    assert!(
        ended.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(query(&destination(&ended), "state"), state);
    assert_eq!(info(&host, text(&active, "access_token")).await, 401);
}
