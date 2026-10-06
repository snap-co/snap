//! Native Chatty OAuth and synchronized Store-backed conversation journey.
//!
//! Owned by the host-only `snap-browser-tests` runner through
//! `tests/browser/src/apps.rs`; Cargo never builds this file standalone. The
//! journey uses real CDP input, carrier-level WebSocket frame observation,
//! direct `ChattyHost::restart` instead of a fixture control endpoint, and
//! `Session::finish` for exception capture plus disposal.

use anyhow::{Context, Result, ensure};
use chromiumoxide::Browser;
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use super::{
    http, matches, query_value, screenshot, set_viewport, unique, wait_url_absent,
    wait_url_contains,
};
use crate::hosts::ChattyHost;
use crate::support;
use crate::ui::{Session, Ui};

pub async fn run(browser: &Browser, filter: &str) -> Result<()> {
    if matches(filter, "websocket mutations access restart") {
        synchronized_conversations(browser).await?;
        return Ok(());
    }
    anyhow::bail!("no chatty cases match filter {filter:?}")
}

#[derive(Clone)]
struct DialogAction {
    accept: bool,
    prompt: Option<String>,
}

/// Answers each JavaScript dialog from a queued expectation (prompt text for
/// renames, plain acceptance for the delete confirmation). Unexpected dialogs
/// are dismissed so the subsequent assertions fail loudly instead of hanging.
async fn dialog_task(page: chromiumoxide::Page, queue: Arc<Mutex<VecDeque<DialogAction>>>) {
    use chromiumoxide::cdp::browser_protocol::page::{
        EventJavascriptDialogOpening, HandleJavaScriptDialogParams,
    };
    use futures::StreamExt;
    let mut stream = match page.event_listener::<EventJavascriptDialogOpening>().await {
        Ok(stream) => stream,
        Err(_) => return,
    };
    while stream.next().await.is_some() {
        let action = queue.lock().unwrap().pop_front().unwrap_or(DialogAction {
            accept: false,
            prompt: None,
        });
        let mut params = HandleJavaScriptDialogParams::new(action.accept);
        params.prompt_text = action.prompt;
        let _ = page.execute(params).await;
    }
}

async fn signup_fresh(ui: &Ui, base: &str, email: &str) -> Result<()> {
    ui.goto(base).await?;
    ui.xpath("//a[contains(normalize-space(.), 'Continue with Authy')]")
        .click()
        .await?;
    ui.button("New here? Create account").click().await?;
    ui.label("Email").fill(email).await?;
    ui.label("Password")
        .fill("Chatty OAuth fixture password")
        .await?;
    ui.button("Create account").click().await?;
    ui.button("Allow").click().await?;
    ui.label("Message Chatty").visible().await
}

async fn send(ui: &Ui, message: &str) -> Result<()> {
    ui.label("Message Chatty").fill(message).await?;
    ui.xpath("//button[@aria-label = 'Send message']")
        .click()
        .await
}

/// "WebSocket mutations synchronize conversations, enforce Access, and
/// survive restart".
async fn synchronized_conversations(browser: &Browser) -> Result<()> {
    let mut host = ChattyHost::start(&support::root(), false).await?;
    // A forged dev origin must not hijack the login redirect: it stays pinned
    // to this host's callback.
    let direct = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let login = direct
        .get(format!("{}/auth/login", host.base))
        .header("x-snap-dev-origin", "http://evil.test")
        .send()
        .await?;
    let location = login
        .headers()
        .get("location")
        .context("missing login redirect")?
        .to_str()
        .unwrap_or_default()
        .to_owned();
    ensure!(
        query_value(&location, "redirect_uri")
            .map(|value| super::decode_query(&value))
            .as_deref()
            == Some(&format!("{}/auth/callback", host.base)),
        "login must pin the callback origin: {location}"
    );
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        let frames = Arc::new(Mutex::new(Vec::<String>::new()));
        {
            use chromiumoxide::cdp::browser_protocol::network::EventWebSocketFrameReceived;
            use futures::StreamExt;
            let mut stream = ui
                .page
                .event_listener::<EventWebSocketFrameReceived>()
                .await?;
            let frames = frames.clone();
            tokio::spawn(async move {
                while let Some(event) = stream.next().await {
                    frames
                        .lock()
                        .unwrap()
                        .push(event.response.payload_data.clone());
                }
            });
        }
        let dialogs = Arc::new(Mutex::new(VecDeque::<DialogAction>::new()));
        tokio::spawn(dialog_task(ui.page.clone(), dialogs.clone()));
        signup_fresh(ui, &host.base, &unique("chatty")).await?;
        send(ui, "Hello").await?;
        ui.locator(".transcript .user-message p")
            .text("Hello")
            .await?;
        let thread_url = wait_url_contains(&ui.page, "?thread=", "new thread").await?;
        let thread = query_value(&thread_url, "thread").unwrap_or_default();
        ensure!(!thread.is_empty(), "thread must be selected");
        let attached = || {
            frames
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| frame.contains("\"Attached\""))
                .count()
        };
        let sockets_before_navigation = attached();
        ui.history(-1).await?;
        wait_url_absent(&ui.page, "?thread=", "thread deselected").await?;
        ui.locator(".transcript .user-message p").count(0).await?;
        ui.history(1).await?;
        wait_url_contains(&ui.page, "?thread=", "thread reselected").await?;
        ui.locator(".transcript .user-message p")
            .text("Hello")
            .await?;
        ensure!(
            attached() == sockets_before_navigation,
            "history navigation must not reattach"
        );
        dialogs.lock().unwrap().push_back(DialogAction {
            accept: true,
            prompt: Some("Renamed conversation".to_owned()),
        });
        ui.button("Rename").click().await?;
        ui.locator(".topbar strong")
            .text("Renamed conversation")
            .await?;
        ui.reload().await?;
        ui.locator(".topbar strong")
            .text("Renamed conversation")
            .await?;
        ui.text("Hello").visible().await?;
        screenshot(&ui.page, "chatty-kit-desktop", 1280, 720, false).await?;
        screenshot(&ui.page, "chatty-kit-mobile", 390, 844, false).await?;
        set_viewport(&ui.page, 1280, 900, false).await?;
        let second = session.page().await?;
        second.goto(&thread_url).await?;
        second.text("Hello").visible().await?;
        send(&second, "From another client").await?;
        ui.text("From another client").visible().await?;
        let _ = second.page.close().await;
        let forbidden = http()?
            .post(format!("{}/api/send", host.base))
            .json(&json!({}))
            .send()
            .await?;
        ensure!(
            !forbidden.status().is_success(),
            "no HTTP application command routes exist"
        );
        other_actor_forbidden(browser, &thread_url).await?;
        // Real restart through the fixture boundary, not a control endpoint.
        host.restart().await?;
        ui.reload().await?;
        ui.text("From another client").visible().await?;
        ensure!(
            frames
                .lock()
                .unwrap()
                .iter()
                .any(|frame| frame.contains("Accepted")),
            "mutations must be accepted after restart"
        );
        dialogs.lock().unwrap().push_back(DialogAction {
            accept: true,
            prompt: None,
        });
        ui.button("Delete").click().await?;
        ui.xpath("//button[normalize-space(.) = 'Renamed conversation']")
            .count(0)
            .await?;
        ui.xpath("//button[@aria-label = 'Sign out']")
            .click()
            .await?;
        ui.button("Confirm sign out").click().await?;
        ui.xpath("//a[contains(normalize-space(.), 'Continue with Authy')]")
            .visible()
            .await?;
        Ok(())
    }
    .await
    .and_then(|()| host.stop());
    session.finish(result).await
}

/// A second actor sees neither the thread nor its messages, and a stolen
/// WebSocket invocation against the victim thread fails before ACK.
async fn other_actor_forbidden(browser: &Browser, thread_url: &str) -> Result<()> {
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        tokio::spawn(dialog_task(
            ui.page.clone(),
            Arc::new(Mutex::new(VecDeque::new())),
        ));
        let base = thread_url
            .split('?')
            .next()
            .context("malformed thread URL")?
            .to_owned();
        signup_fresh(ui, &base, &unique("other")).await?;
        ui.goto(thread_url).await?;
        ui.label("Message Chatty").visible().await?;
        ui.text("Hello").count(0).await?;
        ui.xpath("//button[normalize-space(.) = 'Renamed conversation']")
            .count(0)
            .await?;
        let thread = query_value(thread_url, "thread").unwrap_or_default();
        ui.eval(&format!(
            "window.__stolen = null; void new Promise((resolve, reject) => {{ \
               const socket = new WebSocket(`${{location.origin.replace(/^http/, 'ws')}}/transport`); \
               const timer = setTimeout(() => {{ try {{ socket.close(); }} catch (e) {{}} reject(new Error('Timed out')); }}, 5000); \
               let accepted = false; \
               socket.onopen = () => socket.send(JSON.stringify({{ Connect: {{ bearer: '', client_id: crypto.randomUUID() }} }})); \
               socket.onmessage = event => {{ \
                 const frame = JSON.parse(String(event.data)); \
                 if (frame.Attached) socket.send(JSON.stringify({{ Invoke: {{ id: 1, operation: 'chatty.send', input: {{ thread_id: {}, request_id: 'stolen', message: 'unauthorized' }} }} }})); \
                  if (frame.Event) {{ \
                    const item = frame.Event; \
                   if (item.Accepted) accepted = true; \
                   if (item.Completed) {{ clearTimeout(timer); socket.close(); resolve({{ accepted, failed: 'Err' in item.Completed.outcome }}); }} \
                 }} \
               }}; \
               socket.onerror = () => {{}}; \
             }}).then(result => window.__stolen = result, error => window.__stolen = {{ error: String(error) }});",
            crate::ui::js(&thread)
        ))
        .await?;
        ui.wait("window.__stolen", json!({ "accepted": false, "failed": true }))
            .await?;
        Ok(())
    }
    .await;
    session.finish(result).await
}
