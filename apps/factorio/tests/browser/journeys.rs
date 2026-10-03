//! Factorio native browser journey, owned beside the application tests.
//!
//! Real CLI/UI, Git and OpenCode journeys. It is compiled by the host-only
//! `snap-browser-tests` crate (via `#[path]` from
//! `tests/browser/src/factorio.rs`), never by Cargo's auto-discovered
//! `tests/*.rs` fast gates: this file lives under `tests/browser/`, not as a
//! top-level `tests/*.rs` target.
//!
//! Behavioral parity covers the full original journey:
//! mobile/scroll/drawer/Escape focus/route/deep-link/reload/back-forward,
//! OpenCode V2 fixture semantics (model/permissions/questionnaire/drafts),
//! stranger forbidden before ACK, CLI/Git/claims/candidate/human-approval/
//! restart/recovery/dirty-worktree contracts, and the `factorio-dev`
//! CSS/failed-build-retained-generation/HMR branch.
//!
//! No JavaScript browser controller and no production test seams. The real
//! native host uses scoped Rust OpenCode pipe/executable dependency fixtures.
//! Frontend builds still use Bun. The OpenCode V2
//! contract fixture itself is a native axum service in this file. Restart and
//! build-state use native process control plus direct log reads, not the old
//! `/restart`/`/build-state` HTTP endpoints.

use crate::hosts::AuthyHost;
use crate::support;
use crate::ui::{Session, Ui};
use anyhow::{Context, Result, anyhow, bail, ensure};
use axum::response::IntoResponse;
use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::{
    emulation::SetDeviceMetricsOverrideParams, input::DispatchKeyEventParams,
    input::DispatchKeyEventType, page::HandleJavaScriptDialogParams,
};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{sleep, timeout};

const JOURNEY: &str =
    "fixture-only human acceptance, CLI/UI records, exclusions and restart recovery";
const FIXTURE_SECRET: &str = "factorio-fixture-client-secret-32-characters";
type DialogEvents = chromiumoxide::listeners::EventStream<
    chromiumoxide::cdp::browser_protocol::page::EventJavascriptDialogOpening,
>;

// ---------------------------------------------------------------------------
// Small helpers over the shared ui/support APIs.
// ---------------------------------------------------------------------------

fn lit(value: &str) -> String {
    crate::ui::xpath_literal(value)
}
fn js(value: &str) -> String {
    crate::ui::js(value)
}

async fn set_viewport(ui: &Ui, width: i64, height: i64) -> Result<()> {
    ui.page
        .execute(SetDeviceMetricsOverrideParams::new(
            width, height, 1.0, false,
        ))
        .await?;
    // Let media queries and the shell layout settle; callers poll for the
    // exact condition they need.
    sleep(Duration::from_millis(120)).await;
    Ok(())
}

async fn current_url(ui: &Ui) -> Result<String> {
    let value = ui.eval("location.href").await?;
    Ok(value.as_str().unwrap_or_default().to_owned())
}

async fn invoke(ui: &Ui, operation: &str, input: Value, bearer: &str) -> Result<Value> {
    let expression = format!(
        r#"(function(){{return new Promise((resolve,reject)=>{{
            const operation={op}, input={inp}, bearer={tok};
            const socket=new WebSocket(location.origin.replace(/^http/,"ws")+"/transport");
            let accepted=false;
            const timer=setTimeout(()=>{{socket.close();reject(new Error("WS timeout"));}},10000);
            socket.onopen=()=>socket.send(JSON.stringify({{Connect:{{bearer,client_id:"fixture-"+Math.random()}}}}));
            socket.onmessage=event=>{{
                const frame=JSON.parse(String(event.data));
                if(frame.Attached) socket.send(JSON.stringify({{Invoke:{{id:1,operation,input}}}}));
                if(frame.Failed){{clearTimeout(timer);socket.close();reject(new Error(JSON.stringify(frame.Failed)));}}
                for(const event of frame.Events??[]) {{
                    if(event.Accepted) accepted=true;
                    if(event.Completed){{clearTimeout(timer);socket.close();resolve({{accepted,...event.Completed.outcome}});}}
                }}
            }};
            socket.onerror=()=>{{clearTimeout(timer);reject(new Error("WS error"));}};
        }})}})()"#,
        op = serde_json::to_string(operation)?,
        inp = serde_json::to_string(&input)?,
        tok = serde_json::to_string(bearer)?,
    );
    ui.eval(&expression).await
}

async fn api_session(ui: &Ui) -> Result<Value> {
    ui.eval("fetch('/api/session',{credentials:'same-origin'}).then(r=>r.json())")
        .await
}

async fn fetch_ok(
    ui: &Ui,
    method: &str,
    path: &str,
    headers: Value,
    body: Option<Value>,
) -> Result<bool> {
    let expression = format!(
        "fetch({path},{{\
            method:{method},\
            headers:{headers},\
            credentials:'same-origin',\
            body:{body}\
        }}).then(r=>r.ok)",
        path = js(path),
        method = js(method),
        headers = serde_json::to_string(&headers)?,
        body = match body {
            Some(v) => format!("JSON.stringify({})", serde_json::to_string(&v)?),
            None => "undefined".to_owned(),
        },
    );
    ui.eval(&expression)
        .await?
        .as_bool()
        .context("HTTP probe did not return a response status")
}

async fn select_options(ui: &Ui, xpath: &str, values: &[&str]) -> Result<()> {
    ui.xpath(xpath).count(1).await?;
    let wanted = serde_json::to_string(values)?;
    let probe = format!(
        r#"(() => {{
            const r=document.evaluate({xp},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);
            const e=r.snapshotItem(0); if(!e) return "missing";
            const wanted={wanted};
            for(const o of e.options) o.selected=wanted.includes(o.value);
            e.dispatchEvent(new Event("input",{{bubbles:true}}));
            e.dispatchEvent(new Event("change",{{bubbles:true}}));
            return [...e.selectedOptions].map(o=>o.value).join(",");
        }})()"#,
        xp = js(xpath),
        wanted = wanted,
    );
    let observed = ui.eval(&probe).await?;
    ensure!(
        observed.as_str().unwrap_or_default() == values.join(","),
        "select {xpath} expected {values:?}, observed {observed}"
    );
    Ok(())
}

async fn press_escape(ui: &Ui) -> Result<()> {
    for kind in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
        let mut params = DispatchKeyEventParams::new(kind);
        params.key = Some("Escape".into());
        params.code = Some("Escape".into());
        params.windows_virtual_key_code = Some(27);
        params.native_virtual_key_code = Some(27);
        ui.page.execute(params).await?;
    }
    Ok(())
}

/// The original browser journey accepts every fixture confirmation. Keep that
/// user-visible contract through CDP instead of replacing `window.confirm`.
async fn dialog_listener(ui: &Ui) -> Result<DialogEvents> {
    Ok(ui.page.event_listener().await?)
}

async fn accept_next_dialog(ui: &Ui) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let mut dialogs = dialog_listener(ui).await?;
    let page = ui.page.clone();
    Ok(tokio::spawn(async move {
        let dialog = timeout(Duration::from_secs(10), dialogs.next())
            .await
            .context("waiting for JavaScript dialog")?
            .context("JavaScript dialog listener ended")?;
        ensure!(
            dialog.has_browser_handler,
            "browser cannot handle JavaScript dialog: {:?}",
            dialog
        );
        page.execute(HandleJavaScriptDialogParams::new(true))
            .await?;
        Ok(())
    }))
}

async fn wait_for_dialog(accept: tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    accept.await.context("JavaScript dialog task panicked")?
}

async fn shot(ui: &Ui, name: &str) -> Result<()> {
    if let Some(dir) = support::artifacts() {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        timeout(
            Duration::from_secs(5),
            ui.page.save_screenshot(
                chromiumoxide::page::ScreenshotParams::builder()
                    .full_page(false)
                    .capture_beyond_viewport(false)
                    .build(),
                &path,
            ),
        )
        .await
        .with_context(|| format!("screenshot {name}"))?
        .with_context(|| format!("screenshot {name}"))?;
    }
    Ok(())
}

fn article_with_heading(name: &str) -> String {
    format!(
        "//article[.//*[self::h1 or self::h2 or self::h3 or self::h4][normalize-space(.) = {}]]",
        lit(name)
    )
}
fn drawer_with_heading(name: &str) -> String {
    format!("//dialog[.//h2[normalize-space(.) = {}]]", lit(name))
}

// Accessible-name aware link/button locators. Several Factorio controls expose
// their accessible name through aria-label while showing abbreviated text
// (notably the thread "Back" link and the icon-only drawer close button).
fn link_name(ui: &Ui, name: &str) -> crate::ui::Locator {
    let name = lit(name);
    ui.xpath(&format!(
        "//a[normalize-space(.) = {name} or @aria-label = {name}]"
    ))
}
fn button_name(ui: &Ui, name: &str) -> crate::ui::Locator {
    let name = lit(name);
    ui.xpath(&format!(
        "//button[normalize-space(.) = {name} or @aria-label = {name}]"
    ))
}
fn button_contains(ui: &Ui, text: &str) -> crate::ui::Locator {
    ui.xpath(&format!(
        "//button[contains(normalize-space(.), {})]",
        lit(text)
    ))
}

// ---------------------------------------------------------------------------
// Native OpenCode V2 contract fixture (axum, in-process).
// ---------------------------------------------------------------------------

struct FixtureSession {
    data: Value,
    environment: Value,
    tool: Option<String>,
    messages: Vec<Value>,
    forms: Vec<Value>,
    permissions: Vec<Value>,
    seen: HashSet<String>,
}
struct ControlState {
    sessions: HashMap<String, FixtureSession>,
    watchers: HashMap<String, Vec<tokio::sync::mpsc::UnboundedSender<String>>>,
}
type Shared = Arc<Mutex<ControlState>>;

fn snapshot(session: &FixtureSession) -> Value {
    json!({
        "messages": session.messages,
        "forms": session.forms,
        "permissions": session.permissions,
        "model": session.data.get("model"),
    })
}
fn notify(state: &Shared, id: &str) {
    let payload = {
        let guard = state.lock().unwrap();
        guard
            .sessions
            .get(id)
            .map(|s| format!("{}\n", serde_json::to_string(&snapshot(s)).unwrap()))
    };
    let Some(line) = payload else { return };
    let mut guard = state.lock().unwrap();
    if let Some(sinks) = guard.watchers.get_mut(id) {
        sinks.retain(|s| s.send(line.clone()).is_ok());
    }
}

async fn scoped_tool(tool: &str, body: Value) -> Result<Value> {
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    let command = if action == "read" {
        tool.to_owned()
    } else {
        tool.replace(" intake-read ", " intake-save - ")
    };
    // Exercise the actual supplied command with neither PATH nor Factorio
    // credentials in the environment, exactly like the original fixture.
    let mut child = tokio::process::Command::new("/bin/bash")
        .arg("-c")
        .arg(&command)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawning scoped intake tool")?;
    {
        use tokio::io::AsyncWriteExt;
        child
            .stdin
            .take()
            .context("missing tool stdin")?
            .write_all(serde_json::to_string(&body)?.as_bytes())
            .await?;
    }
    let output = child.wait_with_output().await?;
    ensure!(
        output.status.success(),
        "Fixture scoped CLI failed with an empty environment: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn fixture_save(state: Shared, id: String) -> Result<()> {
    let tool = {
        let guard = state.lock().unwrap();
        guard
            .sessions
            .get(&id)
            .and_then(|s| s.tool.clone())
            .context("Missing explicit intake command")?
    };
    let observed: Value = scoped_tool(&tool, json!({"action":"read"})).await?;
    let intake_id = observed
        .get("intake")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .context("missing intake id")?
        .to_owned();
    let revision = observed
        .get("intake")
        .and_then(|v| v.get("revision"))
        .cloned()
        .unwrap_or(Value::Null);
    let modules = observed
        .get("modules")
        .and_then(Value::as_object)
        .context("missing modules")?;
    let first = modules.keys().next().cloned().unwrap_or_else(|| "a".into());
    let draft_id = format!("{intake_id}-navigation");
    scoped_tool(
        &tool,
        json!({
            "revision": revision,
            "route": "implement",
            "rationale": "The outcome and single-module scope are agreed.",
            "tickets": [{
                "id": draft_id,
                "title": "Improve mobile navigation",
                "description": "Make navigation usable on a phone. Acceptance: ticket links remain visible at 390px.",
                "modules": [first],
                "status": "draft",
                "notes": "",
                "parent": Value::Null,
                "blockers": [],
            }],
        }),
    )
    .await?;
    {
        let mut guard = state.lock().unwrap();
        if let Some(session) = guard.sessions.get_mut(&id) {
            session.messages.push(json!({
                "id": "msg_fixture_answer",
                "role": "assistant",
                "parts": [{"type":"text","text":"I saved a single-module draft. Review it and mark it ready when you want to start."}],
            }));
        }
    }
    notify(&state, &id);
    Ok(())
}

async fn post_request(
    axum::extract::State(state): axum::extract::State<Shared>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let input: Value = match serde_json::from_slice(&body) {
        Ok(input) => input,
        Err(_) => return axum::http::StatusCode::BAD_REQUEST.into_response(),
    };
    let method = input.get("method").and_then(Value::as_str).unwrap_or("");
    let path = input.get("path").and_then(Value::as_str).unwrap_or("");
    let body = input.get("body").cloned().unwrap_or(Value::Null);
    let id = path.split('/').nth(3).unwrap_or("").to_owned();
    if path == "/api/session" && method == "POST" {
        let session_id = body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if session_id.is_empty() {
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
        {
            let mut guard = state.lock().unwrap();
            guard.sessions.insert(
                session_id.clone(),
                FixtureSession {
                    data: body.clone(),
                    environment: json!({}),
                    tool: None,
                    messages: Vec::new(),
                    forms: Vec::new(),
                    permissions: Vec::new(),
                    seen: HashSet::new(),
                },
            );
        }
        notify(&state, &session_id);
        return axum::Json(json!({"data": body})).into_response();
    }
    // Methods operating on an existing session.
    let exists = state.lock().unwrap().sessions.contains_key(&id);
    if !exists {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    if method == "DELETE" {
        {
            let mut guard = state.lock().unwrap();
            guard.sessions.remove(&id);
            guard.watchers.remove(&id);
        }
        return axum::Json(Value::Null).into_response();
    }
    if path.ends_with("/model") {
        {
            let mut guard = state.lock().unwrap();
            if let Some(session) = guard.sessions.get_mut(&id) {
                session.data["model"] = body.get("model").cloned().unwrap_or(Value::Null);
            }
        }
        notify(&state, &id);
    } else if path.ends_with("/environment") {
        {
            let mut guard = state.lock().unwrap();
            if let Some(session) = guard.sessions.get_mut(&id) {
                session.environment = body.get("variables").cloned().unwrap_or(Value::Null);
            }
        }
        notify(&state, &id);
    } else if path.ends_with("/prompt") {
        let prompt_id = body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let fresh = {
            let mut guard = state.lock().unwrap();
            let session = guard.sessions.get_mut(&id).unwrap();
            if session.seen.contains(&prompt_id) {
                false
            } else {
                session.seen.insert(prompt_id.clone());
                if body
                    .get("metadata")
                    .and_then(|m| m.get("factorio_initial"))
                    .is_some()
                {
                    let text = body.get("text").and_then(Value::as_str).unwrap_or("");
                    session.tool = text.split('\n').find_map(|line| {
                        (line.starts_with('\'') && line.contains(" intake-read --credentials "))
                            .then(|| line.to_owned())
                    });
                    session.messages.push(json!({"id": prompt_id, "role": "user", "text": "Improve navigation on my phone"}));
                    session.messages.push(json!({"id": "msg_fixture_question", "role": "assistant", "parts": [{"type":"text","text":"Which navigation outcome matters most?"}]}));
                    session.forms = vec![
                        json!({"id":"frm_scope","title":"Clarify navigation","fields":[
                            {"key":"scope","type":"multiselect","title":"Scope","required":true,"custom":true,"options":[{"value":"known","label":"Known scope"}],"default":["other"]},
                            {"key":"outcome","type":"string","title":"Desired outcome","required":true,"when":[{"key":"scope","op":"eq","value":"other"}]},
                            {"key":"alternate","type":"string","title":"Alternate outcome","default":"stale default","when":[{"key":"scope","op":"neq","value":"other"}]},
                            {"key":"inactive","type":"string","hidden":true,"default":"must not submit","when":[{"key":"scope","op":"neq","value":"other"}]},
                            {"key":"unanswered","type":"string","title":"Optional detail"},
                            {"key":"followup","type":"string","title":"Unanswered follow-up","required":true,"when":[{"key":"unanswered","op":"neq","value":"no"}]},
                        ]}),
                    ];
                    session.permissions =
                        vec![json!({"id":"per_read","action":"read","resources":["crates/a"]})];
                } else {
                    let text = body
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let reply = format!("msg_reply_{}", session.seen.len());
                    session
                        .messages
                        .push(json!({"id": prompt_id, "role": "user", "text": text}));
                    session.messages.push(json!({"id": reply, "role": "assistant", "parts": [{"type":"text","text":"Your additional context is recorded."}]}));
                }
                true
            }
        };
        if fresh {
            notify(&state, &id);
        }
    } else if path.contains("/form/") && path.ends_with("/reply") {
        let answer = body.get("answer").cloned().unwrap_or(Value::Null);
        let scope_ok = answer.get("scope").is_some_and(|v| *v == json!(["other"]));
        let outcome_ok = answer
            .get("outcome")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let keys_ok = answer
            .as_object()
            .is_some_and(|o| o.keys().all(|k| k == "scope" || k == "outcome"));
        if !(scope_ok && outcome_ok && keys_ok) {
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
        {
            let mut guard = state.lock().unwrap();
            if let Some(session) = guard.sessions.get_mut(&id) {
                session.forms.clear();
            }
        }
        if let Err(error) = fixture_save(state.clone(), id.clone()).await {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                axum::Json(json!({"error_description": error.to_string()})),
            )
                .into_response();
        }
        // fixture_save already notified.
    } else if path.contains("/permission/") && path.ends_with("/reply") {
        {
            let mut guard = state.lock().unwrap();
            if let Some(session) = guard.sessions.get_mut(&id) {
                session.permissions.clear();
            }
        }
        notify(&state, &id);
    } else {
        // Interrupt/move and other pass-through calls keep the snapshot live.
        notify(&state, &id);
    }
    let data = state
        .lock()
        .unwrap()
        .sessions
        .get(&id)
        .map(|s| s.data.clone())
        .unwrap_or(Value::Null);
    axum::Json(json!({"data": data})).into_response()
}

async fn get_watch(
    axum::extract::State(state): axum::extract::State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let initial = {
        let guard = state.lock().unwrap();
        guard
            .sessions
            .get(&id)
            .map(|s| format!("{}\n", serde_json::to_string(&snapshot(s)).unwrap()))
    };
    let Some(first) = initial else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    {
        let mut guard = state.lock().unwrap();
        guard.watchers.entry(id).or_default().push(sender);
    }
    // Emit the current snapshot immediately, matching the original fixture.
    let stream = futures::stream::unfold((Some(first), receiver), |(pending, mut rx)| async {
        if let Some(line) = pending {
            return Some((Ok::<_, std::io::Error>(line), (None, rx)));
        }
        rx.recv().await.map(|line| (Ok(line), (None, rx)))
    });
    axum::body::Body::from_stream(stream).into_response()
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Factorio fixture: disposable repo/resources, shim binaries, deployment,
// native host process (or `snap dev` supervisor), and the axum control plane.
// ---------------------------------------------------------------------------

const SETUP_SH: &str = "#!/bin/sh\nif [ \"$FACTORIO_SESSION\" = fail ] && [ ! -f \"$FACTORIO_DATA/permit\" ]; then echo \"fixture setup failure\" >&2; exit 1; fi\nprintf \"%s\" \"$PORT\" > \"$FACTORIO_DATA/port\"\n";

struct FactorioSetup {
    process: Option<support::Process>,
    authy: AuthyHost,
    // Drop order follows declaration order. This guard stops the control server
    // before `_scratch` removes the directory it may still serve from.
    control: support::Task,
    control_addr: std::net::SocketAddr,
    _scratch: tempfile::TempDir,
    _source: Option<support::SourceCopy>,
    deployment: support::Deployment,
    dir: PathBuf,
    developer: PathBuf,
    tcp: String,
    base: String,
    root: PathBuf,
    project_root: PathBuf,
    dev: bool,
}

impl FactorioSetup {
    async fn start_factorio_process(&mut self) -> Result<()> {
        let mut command = support::command(if self.dev {
            self.root.join("target/debug/snap")
        } else {
            self.root.join("target/debug/factorio")
        });
        if self.dev {
            command
                .arg("dev")
                .arg(self.project_root.join("apps/factorio"));
        } else {
            command.arg("serve");
        }
        self.deployment.apply(&mut command);
        command.current_dir(&self.developer);
        command.env(
            "PATH",
            format!(
                "{}/bin:{}",
                self.dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        command.env(
            "FACTORIO_FIXTURE_API",
            format!("http://{}", self.control_addr),
        );
        command.env("FACTORIO_FIXTURE", &self.dir);
        let mut process = support::Process::start(&mut command)?;
        let prefix = if self.dev {
            "Factorio dev "
        } else {
            "Factorio "
        };
        timeout(Duration::from_secs(60), async {
            loop {
                process.alive()?;
                let ready = process.log().contains(prefix)
                    && reqwest::Client::builder()
                        .timeout(Duration::from_secs(1))
                        .build()?
                        .get(format!("{}/api/session", self.base))
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success());
                if ready {
                    break Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .with_context(|| format!("Factorio startup failed: {}", process.log()))??;
        self.process = Some(process);
        Ok(())
    }

    async fn restart(&mut self) -> Result<()> {
        if let Some(process) = self.process.as_mut() {
            process.stop()?;
        }
        self.start_factorio_process().await
    }

    fn generations(&mut self) -> usize {
        self.process
            .as_ref()
            .map(|p| p.log().matches("generation ready").count())
            .unwrap_or(0)
    }

    fn build_failed(&mut self) -> bool {
        self.process.as_ref().is_some_and(|p| {
            p.log()
                .contains("Rebuild failed; previous generation retained")
        })
    }

    async fn shutdown(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.stop();
        }
        let _ = self.authy.stop();
        self.control.stop().await;
    }
}

fn certificates(dir: &Path) -> Result<()> {
    let file = |name: &str| dir.join(name).to_string_lossy().into_owned();
    for args in [
        vec![
            "req".into(),
            "-x509".into(),
            "-newkey".into(),
            "ec".into(),
            "-pkeyopt".into(),
            "ec_paramgen_curve:P-256".into(),
            "-nodes".into(),
            "-days".into(),
            "2".into(),
            "-subj".into(),
            "/CN=Factorio fixture CA".into(),
            "-keyout".into(),
            file("ca-key.pem"),
            "-out".into(),
            file("ca.pem"),
            "-addext".into(),
            "basicConstraints=critical,CA:TRUE".into(),
        ],
        vec![
            "req".into(),
            "-new".into(),
            "-newkey".into(),
            "ec".into(),
            "-pkeyopt".into(),
            "ec_paramgen_curve:P-256".into(),
            "-nodes".into(),
            "-subj".into(),
            "/CN=localhost".into(),
            "-keyout".into(),
            file("server-key.pem"),
            "-out".into(),
            file("server.csr"),
            "-addext".into(),
            "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1".into(),
            "-addext".into(),
            "extendedKeyUsage=serverAuth".into(),
            "-addext".into(),
            "basicConstraints=critical,CA:FALSE".into(),
        ],
        vec![
            "x509".into(),
            "-req".into(),
            "-in".into(),
            file("server.csr"),
            "-CA".into(),
            file("ca.pem"),
            "-CAkey".into(),
            file("ca-key.pem"),
            "-CAcreateserial".into(),
            "-days".into(),
            "2".into(),
            "-copy_extensions".into(),
            "copy".into(),
            "-out".into(),
            file("server.pem"),
        ],
    ] {
        support::run(support::command("openssl").args(args))?;
    }
    Ok(())
}

async fn setup_fixture(dev: bool) -> Result<FactorioSetup> {
    let source = if dev {
        Some(support::SourceCopy::new()?)
    } else {
        None
    };
    let root = support::root();
    let project_root = source
        .as_ref()
        .map(|s| s.root.clone())
        .unwrap_or_else(|| root.clone());
    let control_state: Shared = Arc::new(Mutex::new(ControlState {
        sessions: HashMap::new(),
        watchers: HashMap::new(),
    }));
    let router = axum::Router::new()
        .route("/opencode/request", axum::routing::post(post_request))
        .route("/opencode/watch/{id}", axum::routing::get(get_watch))
        .with_state(control_state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let control_addr = listener.local_addr()?;
    let control = support::Task::new(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let base_port = support::reserve_port()?;
    let base = format!("http://127.0.0.1:{base_port}");
    let first_port = support::reserve_port()?;
    let authy = AuthyHost::start(
        &base,
        json!({
            "FACTORIO_ORIGIN": base,
            "FACTORIO_CLIENT_SECRET": FIXTURE_SECRET,
        }),
    )
    .await?;
    let scratch = support::scratch("factorio-journey-")?;
    let dir = scratch.path().to_owned();
    let repository = dir.join("repo");
    let resources = dir.join("resources");
    std::fs::create_dir_all(repository.join("crates/a"))?;
    std::fs::create_dir_all(repository.join("crates/b"))?;
    std::fs::create_dir_all(dir.join("bin"))?;
    std::fs::write(repository.join("crates/a/file"), "base a")?;
    std::fs::write(repository.join("crates/b/file"), "base b")?;
    {
        let mut git = support::command("git");
        git.arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(&repository);
        support::run(&mut git)?;
        let mut git = support::command("git");
        git.arg("add").arg(".").current_dir(&repository);
        support::run(&mut git)?;
        let mut git = support::command("git");
        git.args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "fixture baseline",
        ])
        .current_dir(&repository);
        support::run(&mut git)?;
    }
    std::os::unix::fs::symlink(
        root.join("target/debug/snap-opencode-fixture"),
        dir.join("bin/opencode"),
    )?;
    certificates(&dir)?;
    let tcp = format!("127.0.0.1:{}", support::reserve_port()?);
    std::fs::write(dir.join("setup.sh"), SETUP_SH)?;
    let bridge = root.join("target/debug/snap-opencode-bridge-fixture");
    ensure!(
        bridge.is_file(),
        "missing production bridge {}",
        bridge.display()
    );
    let repository_config = json!({
        "repository": repository.to_str().unwrap(),
        "mainline": "main",
        "modules": {"a":"crates/a","b":"crates/b"},
        "resources": resources.to_str().unwrap(),
        "first_port": first_port,
        "setup": ["/bin/sh", dir.join("setup.sh").to_str().unwrap()],
        "teardown": [],
    });
    let web_dir = root.join("apps/factorio/dist/development/clients/web");
    ensure!(
        web_dir.join("index.html").is_file(),
        "build Factorio web assets first: {}",
        web_dir.display()
    );
    let config = json!({
        "host": {
            "mode": "development",
            "listen": format!("127.0.0.1:{base_port}"),
            "origin": base,
            "data_dir": dir.to_str().unwrap(),
            "database": "factorio.sqlite",
            "web_dir": web_dir.to_str().unwrap(),
        },
        "app": {
            "tcp": {"listen":tcp,"cert_file":dir.join("server.pem"),"key_file":dir.join("server-key.pem"),"ca_file":dir.join("ca.pem"),"server_name":"localhost"},
            "repository": repository_config,
            "oauth": {"issuer": authy.base, "client_id": "factorio", "client_secret_ref": "oauth.client_secret"},
            "tools": {"bun": bridge.to_str().unwrap(), "opencode": dir.join("bin/opencode").to_str().unwrap(), "bridge": bridge.to_str().unwrap()},
        },
    });
    let secrets = json!({"oauth": {"client_secret": FIXTURE_SECRET}});
    let developer = dir.join("developer");
    let client_project = developer.join("apps/factorio");
    std::fs::create_dir_all(&client_project)?;
    std::fs::write(
        client_project.join("snap.toml"),
        "version=1\napplication='factorio'\n",
    )?;
    let deployment = support::Deployment::create(&dir, config, Some(secrets))?;
    let mut profiles = vec![client_project.join(".snap/development")];
    if dev {
        profiles.push(project_root.join("apps/factorio/.snap/development"));
    }
    for profile in profiles {
        std::fs::create_dir_all(&profile)?;
        for name in ["config.toml", "secrets.enc", "secrets.key"] {
            std::fs::copy(
                deployment.path.parent().unwrap().join(name),
                profile.join(name),
            )?;
        }
    }
    {
        let mut migrate = support::command(root.join("target/debug/factorio"));
        migrate.args(["serve", "--migrate"]).current_dir(&developer);
        deployment.apply(&mut migrate);
        support::run(&mut migrate)?;
    }
    let mut setup = FactorioSetup {
        process: None,
        authy,
        control,
        control_addr,
        _scratch: scratch,
        _source: source,
        deployment,
        dir,
        developer,
        tcp,
        base,
        root,
        project_root,
        dev,
    };
    setup.start_factorio_process().await?;
    Ok(setup)
}

// ---------------------------------------------------------------------------
// Production native CLI driver over the real TLS transport.
// ---------------------------------------------------------------------------

async fn cli(setup: &FactorioSetup, token: &str, args: &[&str]) -> Result<Value> {
    let mut command = tokio::process::Command::new(setup.root.join("target/debug/factorio"));
    command
        .args(args)
        .current_dir(&setup.developer)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("XDG_CONFIG_HOME", setup.dir.join("cli-config"))
        .env("TMPDIR", std::env::var("TMPDIR").unwrap_or_default())
        .env(
            "FACTORIO_OPENCODE",
            setup.dir.join("bin/opencode").to_str().unwrap(),
        )
        .env("FACTORIO_ADDR", &setup.tcp)
        .env("FACTORIO_CA_FILE", setup.dir.join("ca.pem"))
        .env("FACTORIO_SERVER_NAME", "localhost")
        .env("FACTORIO_FIXTURE", &setup.dir)
        .env("FACTORIO_TOKEN", token)
        .env("BUN_CONFIG_DNS", "off")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(15), command.output()).await??;
    if !output.status.success() {
        bail!(
            "cli {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn cli_err(setup: &FactorioSetup, token: &str, args: &[&str]) -> Result<String> {
    match cli(setup, token, args).await {
        Ok(value) => bail!("cli {args:?} unexpectedly succeeded: {value}"),
        Err(error) => Ok(error.to_string()),
    }
}

async fn git(args: &[&str], dir: &Path) -> Result<String> {
    let mut command = support::command("git");
    command.args(args).current_dir(dir);
    support::run(&mut command)
}

// Collect the React error surface the original journey asserted empty:
// every pageerror plus console errors matching the legacy root race.
async fn error_snapshot(ui: &Ui) -> Result<Vec<String>> {
    let value = ui
        .eval(
            "(() => { const all=(window.__snapErrors||[]); \
               return all.filter(m=>/removeChild|already been passed to createRoot/.test(m)||m.startsWith('pageerror:')); })()",
        )
        .await?;
    Ok(value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect())
}

async fn signup(ui: &Ui, email: &str, password: &str) -> Result<()> {
    link_name(ui, "Continue with Authy").click().await?;
    ui.button("New here? Create account").click().await?;
    ui.label("Email").fill(email).await?;
    ui.label("Password").fill(password).await?;
    ui.button("Create account").click().await?;
    ui.button("Allow").click().await?;
    ui.heading("Create your first workspace").visible().await?;
    Ok(())
}

async fn journey_main(ui: &Ui, setup: &mut FactorioSetup) -> Result<(String, String, String)> {
    let base = setup.base.clone();
    set_viewport(ui, 390, 844).await?;
    ui.init(
        "Reflect.deleteProperty(Object.getPrototypeOf(crypto),'randomUUID');\
         window.__snapErrors=[];\
         window.addEventListener('error',e=>window.__snapErrors.push('pageerror:'+(e.message||'error')));\
         {const orig=console.error.bind(console);console.error=(...a)=>{try{window.__snapErrors.push(a.join(' '));}catch{}orig(...a);};}",
    )
    .await?;
    ui.goto(&base).await?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    signup(
        ui,
        &format!("factorio-{stamp}@example.test"),
        "Factorio fixture password",
    )
    .await?;
    ui.button("Create workspace").click().await?;
    ui.xpath(
        "//nav[@aria-label='Workspace' and contains(@class,'mobile-navigation')]//a[normalize-space(.)='Tickets' or @aria-label='Tickets']",
    )
    .click()
    .await?;
    ui.text("No tickets yet").visible().await?;
    ui.xpath(
        "//nav[@aria-label='Workspace' and contains(@class,'mobile-navigation')]//a[normalize-space(.)='Intakes' or @aria-label='Intakes']",
    )
    .click()
    .await?;
    let identity = api_session(ui).await?;
    let csrf = identity
        .get("csrf")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let _mutation_headers = json!({"origin": base, "x-snap-csrf": csrf});
    let workspaces = invoke(ui, "factorio.workspaces", json!({}), "").await?;
    let workspace = workspaces
        .get("Ok")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|w| w.get("id"))
        .and_then(Value::as_str)
        .context("missing workspace")?
        .to_owned();
    for n in 0..6 {
        let created = invoke(
            ui,
            "factorio.command",
            json!({"workspace": workspace, "command": {"command":"ticket","ticket":{"id":format!("preceding-{n}"),"title":format!("Earlier ticket {n}"),"description":"Existing work","modules":["a"],"status":"draft","notes":"","parent":Value::Null,"blockers":[]}}}),
            "",
        )
        .await?;
        ensure!(
            created.get("Ok").is_some_and(|v| v.is_null()),
            "preceding ticket {n}: {created}"
        );
    }
    ui.label("Your idea")
        .fill("Improve navigation on my phone")
        .await?;
    ui.button("Work through this").click().await?;
    ui.text("Which navigation outcome matters most?")
        .visible()
        .await?;
    ui.xpath("//*[@aria-label='OpenCode model']")
        .text("opencode-go/muse-spark-1.3-contributor")
        .await?;
    ui.heading("Tickets").count(0).await?;
    for height in [844, 420, 844] {
        set_viewport(ui, 390, height).await?;
        ui.wait(
            "Math.abs(document.querySelector('.thread-bottom').getBoundingClientRect().bottom-document.querySelector('.mobile-navigation').getBoundingClientRect().top)<2",
            json!(true),
        )
        .await?;
        ui.eval("document.querySelector('.thread-scroll').scrollTop=0")
            .await?;
        ui.button("Send reply").visible().await?;
    }
    ui.button("Allow once").click().await?;
    ui.label("Desired outcome").visible().await?;
    ui.label("Alternate outcome").count(0).await?;
    ui.label("Unanswered follow-up").count(0).await?;
    ui.label("Scope custom choices").fill("").await?;
    let scope_xpath = "//select[@aria-label='Scope']";
    select_options(ui, scope_xpath, &["known"]).await?;
    ui.label("Desired outcome").count(0).await?;
    ui.label("Alternate outcome").visible().await?;
    select_options(ui, scope_xpath, &[]).await?;
    ui.button("Send answers").click().await?;
    ui.xpath(scope_xpath).visible().await?;
    let missing = ui
        .eval(&format!(
            "(() => {{ const r=document.evaluate({},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null); return r.snapshotItem(0)?.validity.valueMissing ?? null; }})()",
            js(scope_xpath)
        ))
        .await?;
    ensure!(
        missing == json!(true),
        "scope should be valueMissing, got {missing}"
    );
    ui.label("Scope custom choices")
        .fill("other, other")
        .await?;
    select_options(ui, scope_xpath, &["known"]).await?;
    select_options(ui, scope_xpath, &[]).await?;
    ui.label("Alternate outcome").count(0).await?;
    ui.label("Desired outcome")
        .fill("Keep ticket links visible on my phone")
        .await?;
    ui.button("Send answers").click().await?;
    let draft_xpath = article_with_heading("Improve mobile navigation");
    let initial_conversation = current_url(ui).await?;
    ui.xpath(
        "//*[contains(@class,'draft-summary')]//a[normalize-space(.)='Improve mobile navigation']",
    )
    .visible()
    .await?;
    ui.xpath(
        "//*[contains(@class,'draft-summary')]//a[normalize-space(.)='Improve mobile navigation']",
    )
    .click()
    .await?;
    ui.wait(
        &format!(
            "(() => {{ const r=document.evaluate({},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null); const e=r.snapshotItem(0); if(!e) return false; const b=e.getBoundingClientRect(); return b.top>=0&&b.top<innerHeight; }})()",
            js(&draft_xpath)
        ),
        json!(true),
    )
    .await?;
    let draft_url = current_url(ui).await?;
    button_contains(ui, "Open tickets").click().await?;
    let drawer = drawer_with_heading("Open tickets");
    ui.xpath(&drawer).visible().await?;
    let first_link = ui
        .eval(&format!(
            "(() => {{ const r=document.evaluate({}+'//a',document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null); return r.snapshotItem(0)?.textContent ?? null; }})()",
            js(&drawer)
        ))
        .await?;
    ensure!(
        first_link
            .as_str()
            .unwrap_or_default()
            .contains("Improve mobile navigation"),
        "drawer first link should mention the draft, got {first_link}"
    );
    ui.xpath(&format!(
        "{drawer}//a[contains(normalize-space(.), 'Earlier ticket 0')]"
    ))
    .click()
    .await?;
    ui.xpath(&drawer).hidden().await?;
    ui.heading("Earlier ticket 0").visible().await?;
    ui.xpath(
        "//nav[@aria-label='Workspace' and contains(@class,'mobile-navigation')]//a[normalize-space(.)='Intakes' or @aria-label='Intakes']",
    )
    .click()
    .await?;
    ui.label("Reply").visible().await?;
    ui.xpath(
        "//nav[@aria-label='Workspace' and contains(@class,'mobile-navigation')]//a[normalize-space(.)='Tickets' or @aria-label='Tickets']",
    )
    .click()
    .await?;
    ui.heading("Earlier ticket 0").visible().await?;
    button_contains(ui, "Open tickets").click().await?;
    press_escape(ui).await?;
    ui.xpath(&drawer).hidden().await?;
    ui.wait(
        "document.activeElement && document.activeElement.textContent.includes('Open tickets')",
        json!(true),
    )
    .await?;
    ui.goto(&draft_url).await?;
    ui.xpath(&draft_xpath).visible().await?;
    ui.reload().await?;
    ui.xpath(&draft_xpath).visible().await?;
    {
        let id = draft_url.rsplit('/').next().unwrap_or_default();
        let decoded = urlencoding_decode(id);
        let origin = draft_url.split('/').take(3).collect::<Vec<_>>().join("/");
        ui.goto(&format!("{origin}/#ticket-{decoded}")).await?;
        ui.wait(
            &format!("location.href === {}", js(&draft_url)),
            json!(true),
        )
        .await?;
    }
    ui.goto(&initial_conversation).await?;
    ui.label("Reply").fill("That is the right scope.").await?;
    ui.button("Send reply").click().await?;
    ui.text("Your additional context is recorded.")
        .visible()
        .await?;
    Ok((workspace, draft_url, initial_conversation))
}

fn urlencoding_decode(value: &str) -> String {
    let mut out = String::new();
    let mut bytes = value.as_bytes().iter().peekable();
    while let Some(&b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next().copied().unwrap_or(b'0');
            let lo = bytes.next().copied().unwrap_or(b'0');
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push((hex(hi) * 16 + hex(lo)) as char);
        } else {
            out.push(b as char);
        }
    }
    out
}

async fn journey_model_stranger(
    browser: &Browser,
    ui: &Ui,
    setup: &FactorioSetup,
    workspace: &str,
) -> Result<(String, String)> {
    let base = setup.base.clone();
    let conversation_url = current_url(ui).await?;
    let intake_id = conversation_url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    let decoded_intake = urlencoding_decode(&intake_id);
    let state = invoke(
        ui,
        "factorio.workspace",
        json!({"workspace": workspace}),
        "",
    )
    .await?;
    let conversation = state
        .get("Ok")
        .and_then(|v| v.get("intakes"))
        .and_then(|v| v.get(&decoded_intake))
        .and_then(|v| v.get("conversation"))
        .and_then(Value::as_str)
        .context("missing intake conversation")?
        .to_owned();
    let session_path = format!("/api/session/{conversation}");
    {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        let body = json!({"method":"POST","path":format!("{session_path}/model"),"body":{"model":{"providerID":"openai","id":"expensive-fixture"}}});
        let response = client
            .post(format!("http://{}/opencode/request", setup.control_addr))
            .json(&body)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "fixture model override failed"
        );
    }
    ui.xpath("//*[@aria-label='OpenCode model']")
        .text("openai/expensive-fixture")
        .await?;
    ui.xpath("//summary[contains(normalize-space(.), 'Drafts and session details')]")
        .click()
        .await?;
    ui.wait(
        "document.querySelector('details.thread-details')?.open",
        json!(true),
    )
    .await?;
    ui.button("Reconnect session").click().await?;
    ui.xpath("//*[@aria-label='OpenCode model']")
        .text("opencode-go/muse-spark-1.3-contributor")
        .await?;
    ui.xpath("//summary[contains(normalize-space(.), 'Drafts and session details')]")
        .click()
        .await?;
    // Stranger: fresh browser context, isolated cookies. Must see no workspaces
    // and be forbidden before ACK on every surface.
    {
        let stranger = Session::new(browser).await?;
        let outcome: Result<()> = async {
            let other = stranger.page().await?;
            other.goto(&base).await?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis();
            link_name(&other, "Continue with Authy").click().await?;
            other.button("New here? Create account").click().await?;
            other.label("Email").fill(&format!("other-{stamp}@example.test")).await?;
            other.label("Password").fill("Other fixture password").await?;
            other.button("Create account").click().await?;
            other.button("Allow").click().await?;
            other.heading("Create your first workspace").visible().await?;
            let empty = invoke(&other, "factorio.workspaces", json!({}), "").await?;
            ensure!(empty.get("Ok").is_some_and(|v| v == &json!([])), "stranger workspaces: {empty}");
            let forbidden = invoke(&other, "factorio.workspace", json!({"workspace": workspace}), "").await?;
            ensure!(forbidden.get("accepted") == Some(&json!(false)) && forbidden.get("Err").is_some(), "stranger workspace should be forbidden before ACK: {forbidden}");
            let events_ok = fetch_ok(&other, "GET", &format!("/api/workspaces/{workspace}/intakes/{decoded_intake}/events"), json!({}), None).await?;
            ensure!(!events_ok, "stranger events should be forbidden");
            let stranger_session = api_session(&other).await?;
            let stranger_csrf = stranger_session.get("csrf").and_then(Value::as_str).unwrap_or("");
            let interrupt_ok = fetch_ok(&other, "POST", &format!("/api/workspaces/{workspace}/intakes/{decoded_intake}/opencode"), json!({"origin": base, "x-snap-csrf": stranger_csrf, "content-type": "application/json"}), Some(json!({"action":"interrupt"}))).await?;
            ensure!(!interrupt_ok, "stranger interrupt should be forbidden");
            Ok(())
        }.await;
        stranger.finish(outcome).await?;
    }
    ui.reload().await?;
    ui.text("Your additional context is recorded.")
        .visible()
        .await?;
    let after = current_url(ui).await?;
    ensure!(
        after == conversation_url,
        "reload should preserve {conversation_url}, got {after}"
    );
    ui.button("Mark implementation tickets ready")
        .click()
        .await?;
    ui.xpath("//*[contains(@class,'draft-summary')]//*[normalize-space(.)='ready']")
        .visible()
        .await?;
    shot(ui, "factorio-thread-ui.png").await?;
    set_viewport(ui, 1280, 900).await?;
    ui.xpath("//*[@aria-label='OpenCode model']")
        .visible()
        .await?;
    shot(ui, "factorio-thread-desktop-ui.png").await?;
    set_viewport(ui, 390, 844).await?;
    link_name(ui, "Back to intakes").click().await?;
    ui.label("Your idea").visible().await?;
    ui.history(-1).await?;
    ui.label("Reply").visible().await?;
    ui.history(1).await?;
    Ok((decoded_intake, conversation_url))
}

async fn journey_tickets_cli(
    ui: &Ui,
    setup: &FactorioSetup,
    workspace: &str,
    draft_url: &str,
    intake_id: &str,
) -> Result<String> {
    ui.goto(draft_url).await?;
    ui.xpath(&format!(
        "{}//*[normalize-space(.)='ready']",
        article_with_heading("Improve mobile navigation")
    ))
    .visible()
    .await?;
    button_contains(ui, "Open tickets").click().await?;
    shot(ui, "factorio-drawer-ui.png").await?;
    button_name(ui, "Close work list").click().await?;
    shot(ui, "factorio-mobile-ui.png").await?;
    set_viewport(ui, 1440, 900).await?;
    ui.xpath("//nav[@aria-label='Ticket list' and not(ancestor::dialog)]")
        .visible()
        .await?;
    shot(ui, "factorio-desktop-ui.png").await?;
    set_viewport(ui, 390, 844).await?;
    let draft = article_with_heading("Improve mobile navigation");
    ui.xpath(&format!(
        "{draft}//button[normalize-space(.)='Edit ticket' or @aria-label='Edit ticket']"
    ))
    .click()
    .await?;
    let description = ui
        .eval(&format!(
            "(() => {{ const r=document.evaluate({},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null); return r.snapshotItem(0)?.value ?? null; }})()",
            js("//textarea[@aria-label='description']")
        ))
        .await?;
    ensure!(
        description
            .as_str()
            .unwrap_or_default()
            .contains("Acceptance:"),
        "ticket description should record acceptance, got {description}"
    );
    let overflow = ui
        .eval("document.documentElement.scrollWidth<=innerWidth")
        .await?;
    ensure!(
        overflow == json!(true),
        "mobile ticket should not overflow horizontally"
    );
    ui.text("More ticket actions").click().await?;
    let delete_dialog = accept_next_dialog(ui).await?;
    ui.xpath(&format!("{draft}//button[normalize-space(.)='Delete']"))
        .click()
        .await?;
    wait_for_dialog(delete_dialog).await?;
    ui.xpath(&draft).count(0).await?;
    ui.xpath("//*[@aria-label='Account']").click().await?;
    ui.button("Sign out").visible().await?;
    ui.button("Create agent token").count(0).await?;
    let token = invoke(ui, "factorio.agent-token", json!({}), "").await?["Ok"]["token"]
        .as_str()
        .context("agent token")?
        .to_owned();
    ensure!(token.len() > 32, "agent token should exceed 32 chars");
    ui.xpath("//*[@aria-label='Account']").click().await?;
    pair_cli(ui, setup, &setup.dir.join("paired-cli.json"), false).await?;
    let paired_status = saved_cli(setup, &setup.dir.join("paired-cli.json"))
        .arg("status")
        .output()
        .await?;
    ensure!(paired_status.status.success(), "paired CLI status failed");
    ensure!(
        serde_json::from_slice::<Value>(&paired_status.stdout)?["config"]["repository"]
            == setup.dir.join("repo").to_str().unwrap(),
        "paired CLI selected wrong repository"
    );
    ui.goto(&format!("{}/tickets", setup.base)).await?;
    // CLI/Git/claims lifecycle through the real production CLI.
    let status = cli(setup, &token, &["status"]).await?;
    ensure!(
        setup.dir.join("cli-config/factory").is_dir(),
        "CLI recovery state escaped the disposable fixture config directory"
    );
    let intake = status
        .get("intakes")
        .and_then(Value::as_object)
        .and_then(|m| m.values().next())
        .cloned()
        .context("missing intake in status")?;
    let intake_id_cli = intake
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(intake_id)
        .to_owned();
    let conversation = intake
        .get("conversation")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let resumed = cli(setup, &token, &["intake", "--resume", &intake_id_cli]).await?;
    ensure!(
        resumed.get("resumed").and_then(Value::as_str) == Some(conversation.as_str()),
        "intake resume should return the conversation, got {resumed}"
    );
    let read = invoke(
        ui,
        "factorio.intake-read",
        json!({"workspace": workspace, "id": intake_id_cli}),
        &token,
    )
    .await?;
    ensure!(
        read.get("Ok")
            .and_then(|v| v.get("intake"))
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            == Some(intake_id_cli.as_str()),
        "intake-read should return the intake, got {read}"
    );
    let base = setup.base.clone();
    ui.goto(&format!("{base}/intakes")).await?;
    ui.xpath("//nav[@aria-label='Conversations']//a")
        .click()
        .await?;
    // The initial OpenCode snapshot scrolls the new thread to its latest
    // message. Wait for it before scrolling a control into view for mouse input.
    ui.text("Connected to OpenCode").visible().await?;
    ui.xpath("//summary[contains(normalize-space(.), 'Drafts and session details')]")
        .click()
        .await?;
    ui.wait(
        "document.querySelector('details.thread-details')?.open",
        json!(true),
    )
    .await?;
    let delete_dialog = accept_next_dialog(ui).await?;
    ui.button("Delete conversation").click().await?;
    wait_for_dialog(delete_dialog).await?;
    ui.label("Your idea").visible().await?;
    let after_delete = cli(setup, &token, &["status"]).await?;
    ensure!(
        after_delete
            .get("intakes")
            .and_then(|v| v.get(&intake_id_cli))
            .is_none(),
        "deleted intake should leave status, got {after_delete}"
    );
    let reread = invoke(
        ui,
        "factorio.intake-read",
        json!({"workspace": workspace, "id": intake_id_cli}),
        &token,
    )
    .await?;
    ensure!(
        reread.get("Err").is_some(),
        "deleted intake-read should fail, got {reread}"
    );
    let replacement = invoke(
        ui,
        "factorio.intake-create",
        json!({"workspace": workspace, "id": intake_id_cli, "description": "Replacement intake"}),
        "",
    )
    .await?;
    ensure!(
        replacement
            .get("Ok")
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            == Some(intake_id_cli.as_str()),
        "replacement intake id, got {replacement}"
    );
    let replacement_conversation = replacement
        .get("Ok")
        .and_then(|v| v.get("conversation"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    ensure!(
        replacement_conversation != conversation,
        "replacement should mint a fresh conversation"
    );
    let deleted = invoke(
        ui,
        "factorio.intake-delete",
        json!({"workspace": workspace, "id": intake_id_cli}),
        "",
    )
    .await?;
    ensure!(
        deleted.get("Ok").is_some_and(|v| v.is_null()),
        "intake-delete: {deleted}"
    );
    // Negative control: the legacy REST command path must stay closed.
    let identity = api_session(ui).await?;
    let csrf = identity.get("csrf").and_then(Value::as_str).unwrap_or("");
    let legacy_ok = fetch_ok(
        ui,
        "POST",
        "/api/command",
        json!({"origin": base, "x-snap-csrf": csrf, "content-type": "application/json"}),
        Some(json!({"command":"delete_ticket","id":"preceding-0"})),
    )
    .await?;
    ensure!(!legacy_ok, "legacy /api/command should stay closed");
    for (id, blockers) in [("first", vec![]), ("dependent", vec!["first"])] {
        let ticket = json!({"id":id,"title":id,"description":"Fixture implementation","modules":["a"],"status":"ready","notes":"","parent":Value::Null,"blockers":blockers});
        std::fs::write(
            setup.dir.join(format!("{id}.json")),
            serde_json::to_string(&ticket)?,
        )?;
        cli(
            setup,
            &token,
            &[
                "ticket",
                setup.dir.join(format!("{id}.json")).to_str().unwrap(),
            ],
        )
        .await?;
    }
    ui.goto(&format!("{base}/tickets/first")).await?;
    ui.xpath("//article[@id='ticket-first']//*[self::h1 or self::h2 or self::h3][normalize-space(.)='first']")
        .visible()
        .await?;
    ui.goto(&format!("{base}/tickets/dependent")).await?;
    ui.link("first (ready)").visible().await?;
    Ok(token)
}

async fn journey_sessions(
    ui: &Ui,
    setup: &mut FactorioSetup,
    workspace: &str,
    token: &str,
) -> Result<()> {
    let base = setup.base.clone();
    let blocked = cli_err(
        setup,
        token,
        &[
            "start",
            "--id",
            "blocked",
            "--tickets",
            "dependent",
            "--modules",
            "a",
            "--",
            "blocked",
        ],
    )
    .await?;
    ensure!(!blocked.is_empty(), "blocked start should fail");
    let a = cli(
        setup,
        token,
        &[
            "start",
            "--id",
            "one",
            "--tickets",
            "first",
            "--modules",
            "a",
            "--",
            "first change",
        ],
    )
    .await?;
    let b = cli(
        setup,
        token,
        &[
            "start",
            "--id",
            "two",
            "--modules",
            "b",
            "--",
            "parallel change",
        ],
    )
    .await?;
    let (aport, adata) = (
        a.get("session")
            .and_then(|v| v.get("port"))
            .cloned()
            .unwrap_or(Value::Null),
        a.get("session")
            .and_then(|v| v.get("data"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    let (bport, bdata) = (
        b.get("session")
            .and_then(|v| v.get("port"))
            .cloned()
            .unwrap_or(Value::Null),
        b.get("session")
            .and_then(|v| v.get("data"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    ensure!(
        aport != bport,
        "parallel sessions need distinct ports: {a} vs {b}"
    );
    ensure!(adata != bdata, "parallel sessions need distinct data dirs");
    for args in [
        vec![
            "start",
            "--id",
            "overlap",
            "--modules",
            "a,b",
            "--",
            "overlap",
        ],
        vec!["start", "--id", "whole", "--modules", "*", "--", "whole"],
    ] {
        let failure = cli_err(setup, token, &args).await?;
        ensure!(
            !failure.is_empty(),
            "conflicting start {args:?} should fail"
        );
    }
    let conversations: Value = serde_json::from_str(&std::fs::read_to_string(
        setup.dir.join("conversations.json"),
    )?)?;
    let a_session = a.get("session").context("missing session one")?;
    let a_conversation = a_session
        .get("conversation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let a_worktree = a_session
        .get("worktree")
        .and_then(Value::as_str)
        .unwrap_or("");
    ensure!(
        conversations
            .get(a_conversation)
            .and_then(|v| v.get("directory"))
            .and_then(Value::as_str)
            == Some(a_worktree),
        "opencode conversation directory should track the worktree: {conversations}"
    );
    std::fs::write(Path::new(a_worktree).join("crates/a/file"), "implemented")?;
    git(&["add", "."], Path::new(a_worktree)).await?;
    git(
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "fixture implementation",
        ],
        Path::new(a_worktree),
    )
    .await?;
    std::fs::write(
        setup.dir.join("evidence.txt"),
        "Fixture checks passed; test review found no unresolved findings.",
    )?;
    let published = cli(
        setup,
        token,
        &[
            "publish",
            "one",
            "--evidence",
            setup.dir.join("evidence.txt").to_str().unwrap(),
        ],
    )
    .await?;
    let candidate = published
        .get("sessions")
        .and_then(|v| v.get("one"))
        .and_then(|v| v.get("candidate"))
        .and_then(|v| v.get("commit"))
        .and_then(Value::as_str)
        .context("missing candidate")?
        .to_owned();
    let accept_early = cli_err(setup, token, &["accept", "one"]).await?;
    ensure!(!accept_early.is_empty(), "unapproved accept should fail");
    let denied = invoke(
        ui,
        "factorio.command",
        json!({"workspace": workspace, "command": {"command":"approve","id":"one","commit":candidate}}),
        token,
    )
    .await?;
    ensure!(
        denied.get("accepted") == Some(&json!(false)) && denied.get("Err").is_some(),
        "agent-token approve must be denied before human ACK: {denied}"
    );
    // This automated account approves disposable fixture code, never user work.
    ui.goto(&format!("{base}/sessions/one")).await?;
    let approval_dialog = accept_next_dialog(ui).await?;
    ui.button("Approve candidate as human").click().await?;
    wait_for_dialog(approval_dialog).await?;
    timeout(Duration::from_secs(30), async {
        loop {
            let approved = cli(setup, token, &["status"])
                .await?
                .get("sessions")
                .and_then(|v| v.get("one"))
                .and_then(|v| v.get("candidate"))
                .and_then(|v| v.get("approval"))
                .is_some();
            if approved {
                break Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .with_context(|| "waiting for human approval")??;
    cli(setup, token, &["accept", "one"]).await?;
    ensure!(
        std::fs::read_to_string(setup.dir.join("repo/crates/a/file"))? == "implemented",
        "accept should integrate the candidate"
    );
    ui.xpath(&format!(
        "{}//*[normalize-space(.)='complete']",
        article_with_heading("one")
    ))
    .visible()
    .await?;
    button_contains(ui, "Open sessions").click().await?;
    ui.xpath(&format!(
        "{}//nav[@aria-label='Session list']//a[strong[normalize-space(.)='one']]",
        drawer_with_heading("Open sessions")
    ))
    .count(0)
    .await?;
    // The completed session hides under the open filter until "all" is shown.
    // Selecting "all" retitles the drawer, so the follow-up lookup must not
    // pin the old heading.
    ui.xpath("//dialog//select").visible().await?;
    {
        let dialog_select = "//dialog//select";
        ui.eval(&format!(
            "(() => {{ const r=document.evaluate({},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null); const e=r.snapshotItem(0); if(!e) return 'missing'; e.value='all'; e.dispatchEvent(new Event('input',{{bubbles:true}})); e.dispatchEvent(new Event('change',{{bubbles:true}})); return e.value; }})()",
            js(dialog_select)
        ))
        .await?;
    }
    ui.xpath("//dialog//nav[@aria-label='Session list']//a[strong[normalize-space(.)='one']]")
        .visible()
        .await?;
    button_name(ui, "Close work list").click().await?;
    ui.goto(&format!("{base}/tickets/dependent")).await?;
    ui.link("first (done)").visible().await?;
    let next = cli(
        setup,
        token,
        &[
            "start",
            "--id",
            "next",
            "--tickets",
            "dependent",
            "--modules",
            "a",
            "--",
            "dependent now ready",
        ],
    )
    .await?;
    ensure!(
        next.get("session")
            .and_then(|v| v.get("phase"))
            .and_then(Value::as_str)
            == Some("active"),
        "dependent should start once its blocker is done: {next}"
    );
    cli(setup, token, &["abandon", "two"]).await?;
    let setup_failure = cli_err(
        setup,
        token,
        &[
            "start",
            "--id",
            "fail",
            "--modules",
            "b",
            "--",
            "failed setup",
        ],
    )
    .await?;
    ensure!(
        setup_failure.contains("fixture setup failure"),
        "fail should hit the fixture hook, got {setup_failure}"
    );
    setup.restart().await?;
    let collision = cli_err(
        setup,
        token,
        &[
            "start",
            "--id",
            "collision",
            "--modules",
            "b",
            "--",
            "collision",
        ],
    )
    .await?;
    ensure!(!collision.is_empty(), "collision after restart should fail");
    let failed = cli(setup, token, &["status"])
        .await?
        .get("sessions")
        .and_then(|v| v.get("fail"))
        .cloned()
        .context("missing failed session")?;
    ensure!(
        failed.get("phase").and_then(Value::as_str) == Some("starting"),
        "fail should wait in starting: {failed}"
    );
    let data = failed.get("data").and_then(Value::as_str).unwrap_or("");
    std::fs::write(Path::new(data).join("permit"), "retry fixture hook")?;
    cli(setup, token, &["recover", "fail"]).await?;
    let recovered = cli(setup, token, &["status"])
        .await?
        .get("sessions")
        .and_then(|v| v.get("fail"))
        .and_then(|v| v.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    ensure!(
        recovered == "active",
        "recover should activate fail, got {recovered}"
    );
    ui.goto(&format!("{base}/sessions/fail")).await?;
    ui.reload().await?;
    ui.xpath(&format!(
        "{}//*[normalize-space(.)='active']",
        article_with_heading("fail")
    ))
    .visible()
    .await?;
    if setup.dev {
        let css = setup.project_root.join("apps/factorio/web/style.css");
        let original_css = std::fs::read_to_string(&css)?;
        std::fs::write(
            &css,
            format!("{original_css}\nbody {{ --factorio-probe: active; }}\n"),
        )?;
        ui.wait(
            "getComputedStyle(document.body).getPropertyValue('--factorio-probe').trim()",
            json!("active"),
        )
        .await?;
        let source = setup.project_root.join("apps/factorio/src/lib.rs");
        let original = std::fs::read_to_string(&source)?;
        std::fs::write(
            &source,
            format!("{original}\ncompile_error!(\"fixture failure\");\n"),
        )?;
        timeout(Duration::from_secs(60), async {
            loop {
                if setup.build_failed() {
                    break Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .with_context(|| "waiting for failed build retention")??;
        ui.reload().await?;
        ui.xpath(&format!(
            "{}//*[normalize-space(.)='active']",
            article_with_heading("fail")
        ))
        .visible()
        .await?;
        let generations = setup.generations();
        std::fs::write(&source, &original)?;
        timeout(Duration::from_secs(60), async {
            loop {
                if setup.generations() > generations {
                    break Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .with_context(|| "waiting for recovered generation")??;
        ui.xpath(&format!(
            "{}//*[normalize-space(.)='active']",
            article_with_heading("fail")
        ))
        .visible()
        .await?;
        let manifest = setup.project_root.join("apps/factorio/snap.toml");
        let settings = std::fs::read_to_string(&manifest)?;
        let before_arguments = setup.generations();
        ensure!(
            settings.contains("args = [\"serve\"]"),
            "fixture server mode declaration missing"
        );
        std::fs::write(
            &manifest,
            settings.replace(
                "args = [\"serve\"]",
                "args = [\"serve\", \"--check-config\"]",
            ),
        )?;
        timeout(Duration::from_secs(15), async {
            loop {
                if setup.process.as_ref().is_some_and(|p| {
                    p.log()
                        .contains("Server arguments changed; restart snap dev")
                }) {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("changed server mode did not require restart")?;
        ensure!(
            setup.generations() == before_arguments,
            "changed server mode published a generation"
        );
        ui.reload().await?;
        ui.xpath(&format!(
            "{}//*[normalize-space(.)='active']",
            article_with_heading("fail")
        ))
        .visible()
        .await?;
        std::fs::write(&manifest, settings)?;
        timeout(Duration::from_secs(60), async {
            while setup.generations() <= before_arguments {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("restored server mode did not rebuild")?;
        let view = setup
            .project_root
            .join("apps/factorio/web/pages/workspace.tsx");
        let original_view = std::fs::read_to_string(&view)?;
        std::fs::write(
            &view,
            original_view.replace("Factorio</span>", "Updated Factorio development UI.</span>"),
        )?;
        ui.text("Updated Factorio development UI.")
            .visible()
            .await?;
    }
    let worktree = cli(setup, token, &["status"])
        .await?
        .get("sessions")
        .and_then(|v| v.get("fail"))
        .and_then(|v| v.get("worktree"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    std::fs::write(Path::new(&worktree).join("dirty"), "keep")?;
    let abandoned = cli(setup, token, &["abandon", "fail"]).await?;
    ensure!(
        abandoned
            .get("sessions")
            .and_then(|v| v.get("fail"))
            .and_then(|v| v.get("phase"))
            .and_then(Value::as_str)
            == Some("abandoned"),
        "abandon should park fail, got {abandoned}"
    );
    ensure!(
        std::fs::read_to_string(Path::new(&worktree).join("dirty"))? == "keep",
        "abandon must preserve dirty worktrees"
    );
    Ok(())
}

async fn pair_cli(
    ui: &Ui,
    setup: &FactorioSetup,
    credentials: &Path,
    override_trust: bool,
) -> Result<Value> {
    let profile = setup
        .developer
        .join("apps/factorio/.snap/development/config.toml");
    let original = std::fs::read_to_string(&profile)?;
    if override_trust {
        std::fs::write(
            &profile,
            original.replace(
                setup.dir.join("ca.pem").to_str().unwrap(),
                setup.dir.join("missing-ca.pem").to_str().unwrap(),
            ),
        )?;
    }
    let result = async {
        let mut command = support::command(setup.root.join("target/debug/factorio"));
        command
            .args(["login", "--credentials"])
            .arg(credentials)
            .current_dir(&setup.developer);
        for key in [
            "FACTORIO_TOKEN",
            "FACTORIO_ADDR",
            "FACTORIO_CA_FILE",
            "FACTORIO_SERVER_NAME",
            "FACTORIO_WORKSPACE",
        ] {
            command.env_remove(key);
        }
        if override_trust {
            command.arg("--ca-file").arg(setup.dir.join("ca.pem"));
        }
        let mut process = support::Process::start(&mut command)?;
        timeout(Duration::from_secs(10), async {
            while !process.log().contains("Waiting for browser approval") {
                process.alive()?;
                sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("CLI pairing did not become ready")??;
        let log = process.log();
        let url = log
            .lines()
            .find_map(|line| line.strip_prefix("Open "))
            .context("pairing URL")?
            .trim();
        ui.goto(url).await?;
        ui.heading("Connect Factorio CLI").visible().await?;
        let code = url.rsplit('/').next().context("pairing code")?;
        ui.locator("code").text(code).await?;
        // A real foreign-origin request must be denied before browser approval.
        let csrf = ui
            .eval("document.querySelector('input[name=csrf]').value")
            .await?;
        let cookie = crate::apps::cookies(&ui.page, url)
            .await?
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        let foreign = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?
            .post(url)
            .header("cookie", cookie)
            .header("origin", "http://foreign.invalid")
            .form(&[("csrf", csrf.as_str().context("pairing csrf")?)])
            .send()
            .await?;
        ensure!(
            !foreign.status().is_success() && !foreign.status().is_redirection(),
            "foreign pairing origin accepted"
        );
        set_viewport(ui, 1440, 900).await?;
        shot(ui, "factory-login-desktop.png").await?;
        set_viewport(ui, 390, 844).await?;
        ensure!(
            ui.eval("document.documentElement.scrollWidth<=innerWidth")
                .await?
                == true,
            "pairing overflows phone"
        );
        shot(ui, "factory-login-mobile.png").await?;
        ui.button("Allow CLI access").click().await?;
        ui.heading("CLI access approved").visible().await?;
        let status = timeout(Duration::from_secs(20), async {
            loop {
                if let Some(status) = process.status()? {
                    break Ok::<_, anyhow::Error>(status);
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("CLI pairing did not finish")??;
        ensure!(status.success(), "CLI pairing failed: {}", process.log());
        let log = process.log();
        let json_start = log.find('{').context("pairing JSON")?;
        let result: Value = serde_json::from_str(&log[json_start..])?;
        ensure!(
            result["logged_in"] == true && result.get("bearer").is_none(),
            "pairing exposes credentials or did not log in"
        );
        let state = saved_cli(setup, credentials)
            .arg("workspaces")
            .output()
            .await?;
        ensure!(
            state.status.success(),
            "saved pairing cannot list workspaces: {}",
            String::from_utf8_lossy(&state.stderr)
        );
        Ok(result)
    }
    .await;
    if override_trust {
        std::fs::write(profile, original)?;
    }
    result
}

fn saved_cli(setup: &FactorioSetup, credentials: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(setup.root.join("target/debug/factorio"));
    command
        .arg("--credentials")
        .arg(credentials)
        .current_dir(&setup.developer)
        .kill_on_drop(true);
    for key in [
        "FACTORIO_TOKEN",
        "FACTORIO_ADDR",
        "FACTORIO_CA_FILE",
        "FACTORIO_SERVER_NAME",
        "FACTORIO_WORKSPACE",
        "SNAP_MASTER_KEY",
    ] {
        command.env_remove(key);
    }
    command
}

async fn authority(setup: &mut FactorioSetup, owner: &str, action: &str) -> Result<Value> {
    use rusqlite::{Connection, params};
    use sha2::{Digest, Sha256};
    if let Some(mut process) = setup.process.take() {
        process.stop()?;
    }
    let result=async {
        let db=Connection::open(setup.dir.join("factorio.sqlite"))?;
        let mut rows=db.prepare("SELECT id,data FROM \"identity.oauth_grants\"")?;
        let entries=rows.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(rows);
        let mut found=None;
        for (id,data) in entries { let data:Value=serde_json::from_str(&data)?; if data["owner"] == owner { found=Some((id,data));break; } }
        let Some((id,mut data))=found else { return Ok(json!({"revoked":true})); };
        if action != "state" {
            if action == "expire-login" { ensure!(db.execute("UPDATE \"factorio.cli\" SET expires=0 WHERE session=?",[&id])? == 1,"missing CLI lifetime"); }
            data["tokens"]["access_expires"]=json!(now()-1);
            db.execute("UPDATE \"identity.oauth_grants\" SET data=? WHERE id=?",params![data.to_string(),id])?;
            if action == "revoke-grant" {
                setup.authy.stop()?;
                let issuer=Connection::open(setup.authy.database())?;
                let hash=Sha256::digest(data["tokens"]["refresh"].as_str().context("refresh token")?);
                ensure!(issuer.execute("UPDATE \"oidc.grants\" SET active=0 WHERE id=(SELECT \"grant\" FROM \"oidc.tokens\" WHERE id=?)",[hash.as_slice()])? == 1,"missing fixture grant");
                drop(issuer);
                setup.authy.restart().await?;
            }
        }
        let expires:i64=db.query_row("SELECT expires FROM \"factorio.cli\" WHERE session=?",[&id],|row|row.get(0))?;
        Ok::<_,anyhow::Error>(json!({"version":data["version"],"access_expires":data["tokens"]["access_expires"],"session_expires":data["expires"],"cli_expires":expires}))
    }.await;
    setup.start_factorio_process().await?;
    result
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn login_journey(browser: &Browser, revoked: bool) -> Result<()> {
    let mut setup = setup_fixture(false).await?;
    let session = Session::new(browser).await?;
    let result = async {
        let ui = &session.ui;
        ui.goto(&setup.base).await?;
        ui.link("Continue with Authy").click().await?;
        ui.button("New here? Create account").click().await?;
        ui.label("Email")
            .fill(&format!("cli-{}@example.test", now()))
            .await?;
        ui.label("Password")
            .fill("Long-lived CLI fixture password")
            .await?;
        ui.button("Create account").click().await?;
        ui.button("Allow").click().await?;
        ui.heading("Create your first workspace").visible().await?;
        let path = setup.dir.join("saved-login.json");
        let granted = pair_cli(ui, &setup, &path, revoked).await?;
        session.ui.page.clone().close().await?;
        let owner = granted["owner"].as_str().context("owner")?;
        if revoked {
            authority(&mut setup, owner, "revoke-grant").await?;
            ensure!(
                !saved_cli(&setup, &path)
                    .arg("workspaces")
                    .output()
                    .await?
                    .status
                    .success(),
                "revoked saved login succeeded"
            );
            ensure!(
                authority(&mut setup, owner, "state").await? == json!({"revoked":true}),
                "revoked session was retained"
            );
        } else {
            let expires = granted["expires"].as_i64().context("expires")?;
            ensure!(
                expires > now() + 29 * 24 * 60 * 60,
                "CLI login shorter than 29 days"
            );
            let saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            ensure!(
                saved.get("refresh").is_none(),
                "saved credentials contain upstream refresh"
            );
            let profile = setup
                .developer
                .join("apps/factorio/.snap/development/config.toml");
            let original = std::fs::read_to_string(&profile)?;
            std::fs::write(&profile, "invalid-development-profile")?;
            let command = saved_cli(&setup, &path).arg("workspaces").output().await;
            std::fs::write(profile, original)?;
            let command = command?;
            ensure!(
                command.status.success()
                    && serde_json::from_slice::<Value>(&command.stdout)? == json!([]),
                "saved login depends on profile"
            );
            let before = authority(&mut setup, owner, "state").await?;
            ensure!(before["cli_expires"] == expires, "CLI lifetime changed");
            authority(&mut setup, owner, "expire-access").await?;
            let mut paths = vec![path.clone()];
            for n in 1..=2 {
                let path = setup.dir.join(format!("saved-login-{n}.json"));
                let mut independent = saved.clone();
                independent["client_id"] = json!(format!("independent-{n}"));
                independent["lifetime"] = Value::Null;
                std::fs::write(&path, independent.to_string())?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                paths.push(path);
            }
            let outputs =
                futures::future::try_join_all(paths.iter().map(|path| async {
                    saved_cli(&setup, path).arg("workspaces").output().await
                }))
                .await?;
            for output in outputs {
                ensure!(
                    output.status.success()
                        && serde_json::from_slice::<Value>(&output.stdout)? == json!([]),
                    "concurrent refresh failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let after = authority(&mut setup, owner, "state").await?;
            ensure!(
                after["version"].as_i64() == before["version"].as_i64().map(|n| n + 1),
                "refresh family rotated more than once"
            );
            ensure!(
                after["access_expires"].as_i64().is_some_and(|n| n > now())
                    && after["cli_expires"] == expires,
                "refresh changed local lifetime"
            );
            ensure!(
                serde_json::from_slice::<Value>(&std::fs::read(&path)?)?["bearer"]
                    == saved["bearer"],
                "refresh changed saved bearer"
            );
            authority(&mut setup, owner, "expire-login").await?;
            ensure!(
                !saved_cli(&setup, &path)
                    .arg("workspaces")
                    .output()
                    .await?
                    .status
                    .success(),
                "expired saved login succeeded"
            );
            ensure!(
                authority(&mut setup, owner, "state").await?["version"] == after["version"],
                "expired login refreshed upstream"
            );
        }
        Ok(())
    }
    .await;
    setup.shutdown().await;
    session.finish(result).await
}

pub async fn run(browser: &Browser, suite: &str, filter: &str) -> Result<()> {
    let mut ran = 0;
    if suite == "factorio" {
        for (name, revoked) in [
            ("saved login refresh after restart and local expiry", false),
            ("revoked refresh grant retires saved login", true),
        ] {
            if filter.is_empty() || name.contains(filter) {
                login_journey(browser, revoked).await?;
                ran += 1;
            }
        }
    }
    if !filter.is_empty() && !JOURNEY.contains(filter) {
        ensure!(ran > 0, "no factorio journeys match {filter:?}");
        return Ok(());
    }
    let dev = match suite {
        "factorio" => false,
        "factorio-dev" => true,
        _ => bail!("unknown factorio suite {suite}"),
    };
    let mut setup = setup_fixture(dev).await?;
    let journey = async {
        let session = Session::new(browser).await?;
        let ui = session.ui.clone();
        let outcome: Result<()> = async {
            let (workspace, draft_url, _initial) = journey_main(&ui, &mut setup).await?;
            let (intake_id, _conversation) =
                journey_model_stranger(browser, &ui, &setup, &workspace).await?;
            // journey_model_stranger leaves the UI on the intake thread after
            // history forward; return to the draft ticket for the CLI phase.
            ui.goto(&draft_url).await?;
            let token =
                journey_tickets_cli(&ui, &setup, &workspace, &draft_url, &intake_id).await?;
            journey_sessions(&ui, &mut setup, &workspace, &token).await?;
            let errors = error_snapshot(&ui).await?;
            ensure!(errors.is_empty(), "page errors: {errors:?}");
            Ok(())
        }
        .await;
        session.finish(outcome).await?;
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {
        result = journey => result,
        _ = tokio::signal::ctrl_c() => Err(anyhow!("factorio journey interrupted")),
    };
    // Always stop scoped processes before TempDirs drop, including on cancel.
    // Process fields precede TempDir fields so Drop order is safe even if this
    // explicit shutdown is skipped by a panic.
    setup.shutdown().await;
    result
}
