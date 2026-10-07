//! Native Authy account, OAuth and scriptless protocol-page journeys.
//!
//! Selected by the app-owned browser consumer in `apps/testing/browser/`.
//! Cargo never builds this file standalone.
//! Every case uses real CDP input, real HTTP fault injection at the carrier
//! boundary, and `Session::finish` for exception capture plus disposal.

use anyhow::{Context, Result, ensure};
use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::network::RequestId;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::{
    contains_text, cookies, encode_query, http, matches, query_value, screenshot, set_viewport,
    submit_until_navigated, unique, wait_url_contains,
};
use crate::hosts::AuthyHost;
use crate::support;
use crate::ui::{Session, Ui};

pub async fn run(browser: &Browser, filter: &str) -> Result<()> {
    let mut ran = 0;
    if matches(filter, "passkey browser ceremony") {
        passkey_ceremony(browser).await?;
        ran += 1;
    }
    if matches(filter, "identity failure retry and deep link") {
        identity_failure_retry(browser).await?;
        ran += 1;
    }
    if matches(filter, "account profile reload logout login") {
        account_profile(browser).await?;
        ran += 1;
    }
    if matches(filter, "oauth consent forced login deny logout") {
        oauth_consent(browser).await?;
        ran += 1;
    }
    if matches(filter, "sign-in failure held submission password") {
        signin_failures(browser).await?;
        ran += 1;
    }
    if matches(filter, "protocol pages styling no javascript") {
        protocol_pages_no_js(browser).await?;
        ran += 1;
    }
    ensure!(ran > 0, "no authy cases match filter {filter:?}");
    Ok(())
}

// The native verifier tests own signature/counter policy. This journey owns the
// browser's JSON conversion, cookie delivery, Wasm SDK wiring and account UI.
async fn passkey_ceremony(browser: &Browser) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::web_authn::{
        AddVirtualAuthenticatorParams, AuthenticatorProtocol, AuthenticatorTransport, EnableParams,
        VirtualAuthenticatorOptions,
    };
    let port = support::reserve_port()?;
    let origin = format!("http://localhost:{port}");
    let mut authy = AuthyHost::start(
        "http://127.0.0.1:1",
        json!({"SNAP_ORIGIN": origin, "SNAP_LISTEN": format!("127.0.0.1:{port}")}),
    )
    .await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        ui.page.execute(EnableParams::default()).await?;
        let options = VirtualAuthenticatorOptions::builder()
            .protocol(AuthenticatorProtocol::Ctap2)
            .transport(AuthenticatorTransport::Usb)
            .has_resident_key(false)
            .has_user_verification(true)
            .is_user_verified(true)
            .automatic_presence_simulation(true)
            .build()
            .map_err(anyhow::Error::msg)?;
        ui.page
            .execute(AddVirtualAuthenticatorParams::new(options))
            .await?;
        ui.goto(&format!("{origin}/sign-in")).await?;
        ui.button("Sign in with a passkey").visible().await?;
        screenshot(&ui.page, "authy-passkey-desktop", 1280, 720, false).await?;
        screenshot(&ui.page, "authy-passkey-mobile", 390, 844, false).await?;
        ui.button("New here? Create account").click().await?;
        let email = unique("passkey");
        ui.label("Email").fill(&email).await?;
        ui.button("Create account with a passkey").click().await?;
        ui.heading("You're signed in").visible().await?;
        contains_text(ui, &email).await?;
        ui.text(&format!("Signed in with {email} (passkey)."))
            .visible()
            .await?;
        ui.button("Sign out").click().await?;
        ui.heading("Sign in").visible().await?;
        // Nonresident hardware must be recoverable by server-side account lookup,
        // even after this browser loses all local hints.
        ui.page.evaluate("localStorage.clear()").await?;
        authy.restart().await?;
        ui.label("Email").fill(&email).await?;
        ui.button("Sign in with a passkey").click().await?;
        ui.heading("You're signed in").visible().await?;
        contains_text(ui, &email).await?;
        ui.reload().await?;
        ui.heading("You're signed in").visible().await?;
        ensure!(
            cookies(&ui.page, &origin)
                .await?
                .iter()
                .any(|cookie| cookie.name == "authy_session" && cookie.http_only),
            "passkey login publishes a browser-managed session cookie"
        );
        Ok(())
    }
    .await;
    session.finish(result).await
}

async fn create_account(ui: &Ui, email: &str, password: &str) -> Result<()> {
    ui.button("New here? Create account").click().await?;
    ui.label("Email").fill(email).await?;
    ui.label("Password").fill(password).await?;
    ui.button("Create account").click().await?;
    ui.label("First name").visible().await
}

async fn fetch_enable(page: &chromiumoxide::Page, pattern: &str) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::fetch::{EnableParams, RequestPattern};
    page.execute(
        EnableParams::builder()
            .pattern(
                RequestPattern::builder()
                    .url_pattern(pattern.to_owned())
                    .build(),
            )
            .build(),
    )
    .await?;
    Ok(())
}

async fn fetch_disable(page: &chromiumoxide::Page) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::fetch::DisableParams;
    page.execute(DisableParams {}).await?;
    Ok(())
}

fn fulfill(
    page: &chromiumoxide::Page,
    id: impl Into<chromiumoxide::cdp::browser_protocol::fetch::RequestId>,
    status: i64,
    body: &str,
) -> impl std::future::Future<Output = Result<()>> {
    use base64::Engine as _;
    use chromiumoxide::cdp::browser_protocol::fetch::{FulfillRequestParams, HeaderEntry};
    let mut params = FulfillRequestParams::new(id, status);
    params.response_headers = Some(vec![HeaderEntry::new("Content-Type", "application/json")]);
    params.body = Some(
        base64::engine::general_purpose::STANDARD
            .encode(body.as_bytes())
            .into(),
    );
    let page = page.clone();
    async move {
        page.execute(params).await?;
        Ok(())
    }
}

fn continue_request(
    page: &chromiumoxide::Page,
    id: impl Into<chromiumoxide::cdp::browser_protocol::fetch::RequestId>,
) -> impl std::future::Future<Output = Result<()>> {
    use chromiumoxide::cdp::browser_protocol::fetch::ContinueRequestParams;
    let params = ContinueRequestParams::new(id);
    let page = page.clone();
    async move {
        page.execute(params).await?;
        Ok(())
    }
}

/// "Identity failure offers retry and a protected deep link survives sign-in".
async fn identity_failure_retry(browser: &Browser) -> Result<()> {
    // No relying party is contacted here; the origin only seeds client
    // registration validation.
    let mut host = AuthyHost::start("http://127.0.0.1:9", json!({})).await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        let sockets = Arc::new(Mutex::new(Vec::<String>::new()));
        {
            use chromiumoxide::cdp::browser_protocol::network::EventWebSocketCreated;
            use futures::StreamExt;
            let mut stream = ui.page.event_listener::<EventWebSocketCreated>().await?;
            let sockets = sockets.clone();
            tokio::spawn(async move {
                while let Some(event) = stream.next().await {
                    sockets.lock().unwrap().push(event.url.clone());
                }
            });
        }
        let blocked = Arc::new(AtomicBool::new(true));
        fetch_enable(&ui.page, "*identity/fetch*").await?;
        {
            use chromiumoxide::cdp::browser_protocol::fetch::EventRequestPaused;
            use futures::StreamExt;
            let mut paused = ui.page.event_listener::<EventRequestPaused>().await?;
            let page = ui.page.clone();
            let blocked = blocked.clone();
            tokio::spawn(async move {
                while let Some(event) = paused.next().await {
                    if !event.request.url.contains("/identity/fetch") {
                        let _ = continue_request(&page, event.request_id.clone()).await;
                        continue;
                    }
                    if blocked.load(Ordering::SeqCst) {
                        let _ = fulfill(&page, event.request_id.clone(), 503, "{}").await;
                    } else {
                        let _ = continue_request(&page, event.request_id.clone()).await;
                    }
                }
            });
        }
        ui.goto(&format!("{}/account", host.base)).await?;
        ui.heading("Unable to open your workspace")
            .visible()
            .await?;
        ui.label("Email").count(0).await?;
        ensure!(
            sockets.lock().unwrap().is_empty(),
            "no socket may open while session resolution fails"
        );
        blocked.store(false, Ordering::SeqCst);
        ui.button("Try again").click().await?;
        let email = unique("kit-deep-link");
        create_account(ui, &email, "kit deep link password").await?;
        wait_url_contains(&ui.page, "/account", "protected deep link").await?;
        support::poll("transport connection", 10, || {
            let sockets = sockets.clone();
            async move {
                Ok(sockets
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|url| url.ends_with("/transport"))
                    .count()
                    == 1)
            }
        })
        .await?;
        fetch_disable(&ui.page).await?;
        Ok(())
    }
    .await
    .and_then(|()| host.stop());
    session.finish(result).await
}

/// Account Store replication across tabs, reload, logout and login.
async fn account_profile(browser: &Browser) -> Result<()> {
    let mut host = AuthyHost::start("http://127.0.0.1:9", json!({})).await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        ui.init(r#"{
          const NativeSocket = WebSocket;
          const nativeFetch = window.fetch;
          window.__identityCarriers = {http:[], connected:[]};
          window.fetch = (...args) => {
            window.__identityCarriers.http.push(String(args[0] instanceof Request ? args[0].url : args[0]));
            return nativeFetch(...args);
          };
          window.__heldSave = {hold:false, frames:[]};
          window.WebSocket = class extends NativeSocket {
            send(frame) {
              const command = JSON.parse(frame);
              if (command.Invoke) window.__identityCarriers.connected.push(command.Invoke.operation);
              return super.send(frame);
            }
            set onmessage(handler) {
              this.__handler = handler;
              super.onmessage = handler && (event => {
                const deliver = () => handler.call(this,event);
                if (window.__heldSave.hold) window.__heldSave.frames.push(deliver);
                else deliver();
              });
            }
            get onmessage() { return this.__handler; }
          };
        }"#).await?;
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        {
            use chromiumoxide::cdp::browser_protocol::network::{
                EventResponseReceived, EventWebSocketCreated,
            };
            use futures::StreamExt;
            let mut responses = ui.page.event_listener::<EventResponseReceived>().await?;
            let enroll_events = events.clone();
            tokio::spawn(async move {
                while let Some(event) = responses.next().await {
                    if event.response.url.ends_with("/identity/enroll")
                        && event.response.status == 200
                    {
                        enroll_events.lock().unwrap().push("identity".to_owned());
                    }
                }
            });
            let mut sockets = ui.page.event_listener::<EventWebSocketCreated>().await?;
            let connect_events = events.clone();
            tokio::spawn(async move {
                while let Some(event) = sockets.next().await {
                    if event.url.ends_with("/transport") {
                        connect_events.lock().unwrap().push("connect".to_owned());
                    }
                }
            });
        }
        let email = unique("profile");
        ui.goto(&host.base).await?;
        ui.button("New here? Create account").click().await?;
        ensure!(
            events.lock().unwrap().is_empty(),
            "no identity or transport traffic before submission"
        );
        ui.label("Email").fill(&email).await?;
        ui.label("Password")
            .fill("a test password for Authy")
            .await?;
        ui.button("Create account").click().await?;
        ui.label("First name").visible().await?;
        support::poll("identity then connect", 10, || {
            let events = events.clone();
            async move { Ok(events.lock().unwrap().len() >= 2) }
        })
        .await?;
        ensure!(
            events.lock().unwrap()[..2] == ["identity", "connect"],
            "identity acquisition must precede transport connection: {:?}",
            events.lock().unwrap()
        );
        let jar = cookies(&ui.page, &host.base).await?;
        let session_cookie = jar
            .iter()
            .find(|cookie| cookie.name == "authy_session")
            .context("missing authy_session cookie")?;
        ensure!(session_cookie.http_only, "session cookie must be HttpOnly");
        ensure!(
            session_cookie
                .same_site
                .as_ref()
                .is_some_and(|s| s.as_ref() == "Lax"),
            "session cookie must be SameSite=Lax"
        );
        ui.text("This session").visible().await?;
        let carriers = ui
            .eval("window.__identityCarriers")
            .await?;
        for operation in ["identity.sessions", "identity.credentials"] {
            ensure!(
                carriers["connected"].as_array().unwrap().iter().any(|v| v == operation),
                "{operation} must use the identified connection: {carriers}"
            );
        }
        ensure!(
            !carriers["http"].as_array().unwrap().iter().any(|v| {
                let url = v.as_str().unwrap();
                url.ends_with("/identity/sessions") || url.ends_with("/identity/credentials")
            }),
            "ordinary Identity queries must not use HTTP: {carriers}"
        );
        let second = session.page().await?;
        second.goto(&host.base).await?;
        second.label("First name").visible().await?;
        for (index, (width, height, name)) in [(1100,900,"Desktop"),(390,844,"Mobile")].into_iter().enumerate() {
            set_viewport(&ui.page, width, height, false).await?;
            ui.label("First name").fill(name).await?;
            ui.label("Last name").fill("Person").await?;
            ui.eval(r#"(() => {
              const field=document.querySelector('#profile-name'), shell=document.querySelector('.auth-shell');
              const button=field.form.querySelector('button[type=submit]');
              const sessions=[...document.querySelectorAll('section')].find(s=>s.querySelector('h2')?.textContent==='Sessions');
              const initialShell=shell.getBoundingClientRect(), initialButton=button.getBoundingClientRect();
              const initialSessionsTop=sessions.getBoundingClientRect().top+scrollY;
              const result={removed:false,shellHeightChange:0,buttonWidthChange:0,sessionsPositionChange:0};
              const observer=new MutationObserver(records=>{
                result.removed ||= records.some(r=>[...r.removedNodes].some(n=>n===field||n.contains(field)));
                result.shellHeightChange=Math.max(result.shellHeightChange,Math.abs(shell.getBoundingClientRect().height-initialShell.height));
                result.buttonWidthChange=Math.max(result.buttonWidthChange,Math.abs(button.getBoundingClientRect().width-initialButton.width));
                result.sessionsPositionChange=Math.max(result.sessionsPositionChange,Math.abs(sessions.getBoundingClientRect().top+scrollY-initialSessionsTop));
              });
              observer.observe(document.getElementById('root'),{subtree:true,childList:true,characterData:true,attributes:true});
              window.__saveStability={result,observer}; window.__heldSave.hold=true;
            })()"#).await?;
            let held = async {
                ui.button("Save profile").click().await?;
                ui.locator("[role=status]").contains("Saving").await?;
                ui.locator("button[type=submit]").enabled(false).await?;
                ui.eval("new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))").await?;
                Ok::<_,anyhow::Error>(())
            }.await;
            ui.eval("window.__heldSave.hold=false; for (const send of window.__heldSave.frames.splice(0)) send()").await?;
            held?;
            ui.text(&format!("Saved revision {}", index+2)).visible().await?;
            ui.button("Save profile").enabled(true).await?;
            second.label("First name").value(name).await?;
            second.label("Last name").value("Person").await?;
            screenshot(&ui.page, if index == 0 { "authy-store-profile-desktop" } else { "authy-store-profile-mobile" }, width, height, false).await?;
            let changes=ui.eval("window.__saveStability.observer.disconnect(); window.__saveStability.result").await?;
            ensure!(changes["removed"] == false, "Saving unmounted editor: {changes}");
            for key in ["shellHeightChange","buttonWidthChange","sessionsPositionChange"] {
                ensure!(changes[key].as_f64().context("layout measurement")? <= 1.0, "Saving changed {key}: {changes}");
            }
        }
        ui.reload().await?;
        ui.label("First name").value("Mobile").await?;
        ui.label("Last name").value("Person").await?;
        second.label("First name").value("Mobile").await?;
        ui.text(&format!("Signed in with {email} (password)."))
            .visible()
            .await?;
        ui.button("Sign out").click().await?;
        ui.label("Email").visible().await?;
        second.label("First name").hidden().await?;
        ui.label("Email").fill(&email).await?;
        ui.label("Password")
            .fill("a test password for Authy")
            .await?;
        ui.button("Sign in").click().await?;
        ui.label("First name").value("Mobile").await?;
        Ok(())
    }
    .await
    .and_then(|()| host.stop());
    session.finish(result).await
}

fn authorize_query(rp: &str, extra: &[(&str, &str)]) -> String {
    let callback = format!("{rp}/auth/callback");
    let mut pairs = vec![
        ("client_id", "chatty"),
        ("redirect_uri", callback.as_str()),
        ("response_type", "code"),
        ("scope", "openid profile email"),
        ("state", "browser-state"),
        ("nonce", "browser-nonce"),
        (
            "code_challenge",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        ),
        ("code_challenge_method", "S256"),
    ];
    pairs.extend_from_slice(extra);
    encode_query(&pairs)
}

/// Spawns a dummy relying party that answers every path with a callback
/// fixture page for observing real OAuth redirects.
async fn dummy_rp() -> Result<(String, support::Task)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::Router::new().fallback(axum::routing::get(|| async {
                axum::response::Html("OAuth callback fixture")
            })),
        )
        .await;
    });
    Ok((origin, support::Task::new(server)))
}

/// "OAuth returns through password login and explicit consent; forced login
/// asks again".
async fn oauth_consent(browser: &Browser) -> Result<()> {
    let (rp, mut rp_server) = dummy_rp().await?;
    let mut host = AuthyHost::start(&rp, json!({})).await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        let authorize = Arc::new(Mutex::new(Vec::<(RequestId, i64)>::new()));
        let consent_post = Arc::new(Mutex::new(None::<RequestId>));
        let authorize_origin = Arc::new(Mutex::new(None::<String>));
        {
            use chromiumoxide::cdp::browser_protocol::network::{
                EventRequestWillBeSent, EventResponseReceived,
            };
            use futures::StreamExt;
            let mut responses = ui.page.event_listener::<EventResponseReceived>().await?;
            {
                let authorize = authorize.clone();
                tokio::spawn(async move {
                    while let Some(event) = responses.next().await {
                        if event.response.url.contains("/oauth/authorize") {
                            authorize
                                .lock()
                                .unwrap()
                                .push((event.request_id.clone(), event.response.status));
                        }
                    }
                });
            }
            let mut sent = ui.page.event_listener::<EventRequestWillBeSent>().await?;
            let authorize_origin = authorize_origin.clone();
            let redirect_hops = authorize.clone();
            let post_id = consent_post.clone();
            tokio::spawn(async move {
                while let Some(event) = sent.next().await {
                    // Redirect hops surface no responseReceived event here, so
                    // the consent 303 is read off the follow-up request's
                    // redirect metadata.
                    if let Some(redirect) = event.redirect_response.as_ref()
                        && redirect.url.contains("/oauth/authorize")
                    {
                        redirect_hops
                            .lock()
                            .unwrap()
                            .push((event.request_id.clone(), redirect.status));
                    }
                    if event.request.method == "POST"
                        && event.request.url.contains("/oauth/authorize")
                    {
                        *post_id.lock().unwrap() = Some(event.request_id.clone());
                        let origin =
                            event
                                .request
                                .headers
                                .inner()
                                .as_object()
                                .and_then(|headers| {
                                    headers.iter().find_map(|(name, value)| {
                                        name.eq_ignore_ascii_case("origin")
                                            .then(|| value.as_str().unwrap_or_default().to_owned())
                                    })
                                });
                        *authorize_origin.lock().unwrap() = origin;
                    }
                }
            });
        }
        let email = unique("oauth-browser");
        let password = "browser OIDC password";
        let parameters = authorize_query(&rp, &[]);
        ui.goto(&format!("{}/oauth/authorize?{parameters}", host.base))
            .await?;
        // A fresh OAuth signup lands on explicit consent, not the account page.
        ui.button("New here? Create account").click().await?;
        ui.label("Email").fill(&email).await?;
        ui.label("Password").fill(password).await?;
        ui.button("Create account").click().await?;
        ui.heading("Authorize application").visible().await?;
        contains_text(ui, "Chatty is requesting access").await?;
        ui.text(&email).visible().await?;
        ui.text("See your email address").visible().await?;
        ui.button("Allow").click().await?;
        let redirect = wait_url_contains(&ui.page, "/auth/callback", "OAuth callback").await?;
        ensure!(
            query_value(&redirect, "code").is_some_and(|code| !code.is_empty()),
            "callback must carry a code"
        );
        ensure!(
            query_value(&redirect, "state").as_deref() == Some("browser-state"),
            "callback must echo state"
        );
        support::poll("consent POST 303", 10, || {
            let authorize = authorize.clone();
            let consent_post = consent_post.clone();
            async move {
                let post_id = consent_post.lock().unwrap().clone();
                let statuses = authorize.lock().unwrap();
                let status = post_id
                    .as_ref()
                    .and_then(|id| statuses.iter().find(|(request, _)| request == id))
                    .map(|(_, status)| *status);
                if let Some(status) = status {
                    ensure!(status == 303, "consent POST must answer 303, got {status}");
                    return Ok(true);
                }
                Ok(false)
            }
        })
        .await
        .with_context(|| {
            format!(
                "consent POST must answer 303, origin {:?}, observed {:?}",
                authorize_origin.lock().unwrap(),
                authorize.lock().unwrap()
            )
        })?;
        let forced = authorize_query(&rp, &[("prompt", "login")]);
        ui.goto(&format!("{}/oauth/authorize?{forced}", host.base))
            .await?;
        ui.label("Password").visible().await?;
        ui.label("Email").fill(&email).await?;
        ui.label("Password").fill(password).await?;
        ui.button("Sign in").click().await?;
        ui.button("Deny").click().await?;
        wait_url_contains(&ui.page, "error=access_denied", "denied redirect").await?;
        let logout = encode_query(&[
            ("client_id", "chatty"),
            ("post_logout_redirect_uri", &format!("{rp}/auth/logged-out")),
            ("state", "logout-state"),
        ]);
        ui.goto(&format!("{}/oauth/logout?{logout}", host.base))
            .await?;
        ui.heading("Sign out of Authy").visible().await?;
        ui.button("Confirm sign out").click().await?;
        let landed = wait_url_contains(&ui.page, "/auth/logged-out", "logout redirect").await?;
        ensure!(
            query_value(&landed, "state").as_deref() == Some("logout-state"),
            "logout must echo state"
        );
        ui.goto(&host.base).await?;
        ui.label("Email").visible().await?;
        Ok(())
    }
    .await
    .and_then(|()| host.stop());
    rp_server.stop().await;
    session.finish(result).await
}

/// "sign-in explains failures, prevents repeat submissions and reveals
/// passwords".
async fn signin_failures(browser: &Browser) -> Result<()> {
    let mut host = AuthyHost::start("http://127.0.0.1:9", json!({})).await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        let sockets = Arc::new(Mutex::new(Vec::<String>::new()));
        {
            use chromiumoxide::cdp::browser_protocol::network::EventWebSocketCreated;
            use futures::StreamExt;
            let mut stream = ui.page.event_listener::<EventWebSocketCreated>().await?;
            let sockets = sockets.clone();
            tokio::spawn(async move {
                while let Some(event) = stream.next().await {
                    sockets.lock().unwrap().push(event.url.clone());
                }
            });
        }
        ui.goto(&host.base).await?;
        ui.label("Email").fill("reader@example.test").await?;
        ui.label("Password").fill("incorrect password").await?;
        ui.xpath("//button[@aria-label = 'Show password']")
            .click()
            .await?;
        ui.wait(
            "document.querySelector('#password')?.getAttribute('type') ?? null",
            json!("text"),
        )
        .await?;
        ui.xpath("//button[@aria-label = 'Hide password']")
            .click()
            .await?;
        // Hold the credential submission at the carrier boundary: the request
        // stays paused until released, then fails once with the same
        // Transport completion envelope the real host would emit.
        let requests = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::watch::channel(false));
        fetch_enable(&ui.page, "*identity/acquire*").await?;
        {
            use chromiumoxide::cdp::browser_protocol::fetch::EventRequestPaused;
            use futures::StreamExt;
            let mut paused = ui.page.event_listener::<EventRequestPaused>().await?;
            let page = ui.page.clone();
            let requests = requests.clone();
            let release = release.clone();
            tokio::spawn(async move {
                while let Some(event) = paused.next().await {
                    if !event.request.url.contains("/identity/acquire") {
                        let _ = continue_request(&page, event.request_id.clone()).await;
                        continue;
                    }
                    if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                        let mut rx = release.1.clone();
                        let _ = rx.wait_for(|open| *open).await;
                    }
                    let id = event
                        .request
                        .headers
                        .inner()
                        .as_object()
                        .and_then(|headers| {
                            headers.iter().find_map(|(name, value)| {
                                name.eq_ignore_ascii_case("x-snap-operation-id")
                                    .then(|| value.as_str().unwrap_or("1").to_owned())
                            })
                        })
                        .and_then(|id| id.parse::<u64>().ok())
                        .unwrap_or(1);
                    let _ = fulfill(
                        &page,
                        event.request_id.clone(),
                        401,
                        &json!({ "Completed": { "id": id, "outcome": { "Err": "InvalidBearer" } } })
                            .to_string(),
                    )
                    .await;
                }
            });
        }
        ui.button("Sign in").click().await?;
        let entered = async {
            ui.button("Signing in…").enabled(false).await?;
            ui.wait(
                "document.querySelector('#email')?.getAttribute('readonly') ?? null",
                json!(""),
            )
            .await?;
            ui.label("Password").click().await?;
            ui.wait(
                "document.activeElement === document.querySelector('#password')",
                json!(true),
            )
            .await?;
            press_enter(&ui.page).await?;
            ui.xpath("//p[@role = 'status']")
                .text("Checking your sign-in details…")
                .await
        }
        .await;
        release.0.send(true)?;
        entered?;
        fetch_disable(&ui.page).await?;
        ui.xpath("//p[@role = 'alert']")
            .text("The email or password is incorrect. Check both and try again.")
            .await?;
        ensure!(
            requests.load(Ordering::SeqCst) == 1,
            "held submission must not repeat: {}",
            requests.load(Ordering::SeqCst)
        );
        ensure!(
            sockets.lock().unwrap().is_empty(),
            "failed sign-in must not open a socket"
        );
        ui.label("Password").value("incorrect password").await?;
        ui.button("Sign in").enabled(true).await?;
        screenshot(&ui.page, "authy-signin-desktop", 1280, 720, false).await?;
        set_viewport(&ui.page, 390, 844, false).await?;
        ui.button("New here? Create account").click().await?;
        contains_text(ui, "Use a unique password").await?;
        ui.label("Password").fill("short").await?;
        ui.button("Create account").click().await?;
        ui.xpath("//p[@role = 'alert']")
            .contains("Choose a longer password")
            .await?;
        ui.wait(
            "document.documentElement.scrollWidth <= window.innerWidth",
            json!(true),
        )
        .await?;
        screenshot(&ui.page, "authy-signup-mobile", 390, 844, false).await?;
        Ok(())
    }
    .await
    .and_then(|()| host.stop());
    session.finish(result).await
}

async fn press_enter(page: &chromiumoxide::Page) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchKeyEventParams, DispatchKeyEventType,
    };
    for kind in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
        page.execute(
            DispatchKeyEventParams::builder()
                .r#type(kind)
                .key("Enter")
                .code("Enter")
                .text("\r")
                .build()
                .map_err(|error| anyhow::anyhow!(error))?,
        )
        .await?;
    }
    Ok(())
}

/// "auth protocol pages share sign-in styling and work without JavaScript".
async fn protocol_pages_no_js(browser: &Browser) -> Result<()> {
    let (rp, mut rp_server) = dummy_rp().await?;
    let mut host = AuthyHost::start(&rp, json!({})).await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        use chromiumoxide::cdp::browser_protocol::emulation::SetScriptExecutionDisabledParams;
        let script = async |enabled: bool| -> Result<()> {
            ui.page
                .execute(SetScriptExecutionDisabledParams::new(enabled))
                .await?;
            Ok(())
        };
        // Enroll over plain HTTP with the canonical Origin, then carry the
        // issued cookie into the scriptless browser context.
        let email = unique("static-auth");
        let enrolled = http()?
            .post(format!("{}/identity/enroll", host.base))
            .header("origin", &host.base)
            .json(&json!({ "email": email, "password": "static auth fixture password" }))
            .send()
            .await?;
        ensure!(enrolled.status().is_success(), "enrollment must succeed");
        let cookie = enrolled
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .filter_map(|pair| pair.split_once('='))
            .find(|(name, _)| name.trim() == "authy_session")
            .map(|(_, value)| value.trim().to_owned())
            .context("missing authy_session cookie")?;
        {
            use chromiumoxide::cdp::browser_protocol::network::{CookieParam, SetCookiesParams};
            ui.page
                .execute(SetCookiesParams::new(vec![
                    CookieParam::builder()
                        .name("authy_session")
                        .value(cookie.clone())
                        .url(host.base.clone())
                        .build()
                        .map_err(|error| anyhow::anyhow!(error))?,
                ]))
                .await?;
        }
        let parameters = encode_query(&[
            ("client_id", "chatty"),
            ("redirect_uri", &format!("{rp}/auth/callback")),
            ("response_type", "code"),
            ("scope", "openid profile email"),
            ("state", "static-state"),
            ("nonce", "static-nonce"),
            ("prompt", "consent"),
            (
                "code_challenge",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            ),
            ("code_challenge_method", "S256"),
        ]);
        let authorize = format!("{}/oauth/authorize?{parameters}", host.base);
        let rendered = http()?
            .get(&authorize)
            .header("cookie", format!("authy_session={cookie}"))
            .send()
            .await?;
        let csp = rendered
            .headers()
            .get("content-security-policy")
            .context("missing content-security-policy")?
            .to_str()
            .unwrap_or_default()
            .to_owned();
        ensure!(
            csp.contains("default-src 'none'"),
            "consent page must lock down scripts: {csp}"
        );
        script(true).await?;
        set_viewport(&ui.page, 390, 844, false).await?;
        ui.goto(&authorize).await?;
        // Page scripts never run here: evaluation is re-enabled only to read
        // the static markup, and disabled again before submitting forms.
        script(false).await?;
        ui.xpath("//a[@aria-label = 'Authy home']")
            .visible()
            .await?;
        ui.text("Sign you in").visible().await?;
        ui.text("Read your profile").visible().await?;
        ui.text(&rp).visible().await?;
        ui.wait(
            "getComputedStyle(document.querySelector('.auth-shell')).backgroundColor",
            json!("rgb(25, 30, 39)"),
        )
        .await?;
        ui.wait(
            "document.documentElement.scrollWidth <= window.innerWidth",
            json!(true),
        )
        .await?;
        screenshot(&ui.page, "authy-consent-mobile", 390, 844, false).await?;
        set_viewport(&ui.page, 1100, 900, false).await?;
        screenshot(&ui.page, "authy-consent-desktop", 1100, 900, false).await?;
        script(true).await?;
        submit_until_navigated(
            &ui.page,
            "//button[normalize-space(.) = 'Deny']",
            "error=access_denied",
            "denied redirect",
        )
        .await?;
        let logout = encode_query(&[
            ("client_id", "chatty"),
            ("post_logout_redirect_uri", &format!("{rp}/auth/logged-out")),
            ("state", "static-logout"),
        ]);
        ui.goto(&format!("{}/oauth/logout?{logout}", host.base))
            .await?;
        script(false).await?;
        ui.heading("Sign out of Authy").visible().await?;
        ui.link("Stay signed in").visible().await?;
        script(true).await?;
        let landed = submit_until_navigated(
            &ui.page,
            "//button[normalize-space(.) = 'Confirm sign out']",
            "/auth/logged-out",
            "logout redirect",
        )
        .await?;
        ensure!(
            query_value(&landed, "state").as_deref() == Some("static-logout"),
            "logout must echo state, landed on {landed}"
        );
        let invalid = http()?
            .get(format!("{}/oauth/resume?request=expired", host.base))
            .send()
            .await?;
        ensure!(
            invalid.status().as_u16() == 400,
            "expired resume must be 400"
        );
        ui.goto(&format!("{}/oauth/resume?request=expired", host.base))
            .await?;
        script(false).await?;
        ui.heading("We couldn't complete this request")
            .visible()
            .await?;
        ui.link("Return to Authy").visible().await?;
        let machine = http()?
            .get(format!("{}/oauth/resume?request=expired", host.base))
            .header("accept", "application/json")
            .send()
            .await?;
        ensure!(
            machine
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .contains("application/json"),
            "machine clients must receive JSON errors"
        );
        Ok(())
    }
    .await
    .and_then(|()| host.stop());
    rp_server.stop().await;
    session.finish(result).await
}
