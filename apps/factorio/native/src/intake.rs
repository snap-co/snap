//! Authenticated, session-scoped OpenCode adapter. No arbitrary API proxy and no
//! OpenCode service credentials in browser responses. Store borrows end before IO.
use super::*;
use axum::extract::Path;
use axum::response::sse::{Event, KeepAlive, Sse};
use factorio::intake::{Drafts, Intake};
use std::{
    convert::Infallible,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

fn bridge() -> String {
    std::env::var("FACTORIO_OPENCODE_BRIDGE").unwrap_or_else(|_| {
        let packaged = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("opencode-bridge.js");
        if packaged.is_file() {
            packaged.to_string_lossy().into_owned()
        } else {
            "scripts/factorio-opencode.ts".into()
        }
    })
}
async fn process(input: Value) -> Result<tokio::process::Child, String> {
    let mut child = Command::new(std::env::var("FACTORIO_BUN").unwrap_or_else(|_| "bun".into()))
        .arg(bridge())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("Missing bridge input")?
        .write_all(input.to_string().as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(child)
}
async fn api(method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    let child = process(json!({"method":method,"path":path,"body":body})).await?;
    let output = tokio::time::timeout(Duration::from_secs(40), child.wait_with_output())
        .await
        .map_err(|_| "OpenCode timed out")?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(
            "OpenCode unavailable. Check its service and model configuration, then retry.".into(),
        );
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "Invalid OpenCode response".into())
}
fn error(message: impl ToString) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error_description":message.to_string()})),
    )
        .into_response()
}
fn actor(s: &rp::Session) -> Actor<'_> {
    Actor {
        session: &s.id,
        human: false,
        now: now(),
    }
}
fn owned(app: &App, s: &rp::Session, id: &str) -> Result<Intake, Error> {
    app.oauth.run("intake.read", |tx| {
        let who = rp::lease(tx, &s.id, now())?;
        let w = factorio::load(tx)?;
        let item = w.intakes.get(id).ok_or(Error::NotFound)?;
        if item.owner != who.owner {
            return Err(Error::NotFound);
        }
        Ok(item.clone())
    })
}

async fn configure(app: &App, s: &rp::Session, item: &Intake) -> Result<(), String> {
    let w = app.workspace().map_err(|e| format!("{e:?}"))?;
    let path = format!("/api/session/{}", item.conversation);
    let model = match std::env::var("FACTORIO_INTAKE_MODEL") {
        Ok(value) => {
            let (provider, id) = value
                .split_once('/')
                .filter(|(p, i)| !p.is_empty() && !i.is_empty())
                .ok_or("FACTORIO_INTAKE_MODEL must be provider/model")?;
            Some(json!({"providerID":provider,"id":id}))
        }
        Err(_) => None,
    };
    let existing = match api("GET", &path, None).await {
        Ok(v)=>v,
        Err(_)=>api("POST", "/api/session", Some(json!({"id":item.conversation,"model":model,"title":format!("Intake: {}", item.description.chars().take(70).collect::<String>()),"location":{"directory":w.config.repository},"metadata":{"factorio_intake":item.id},"permissions":[{"action":"edit","resource":"*","effect":"deny"}]}))).await?
    };
    if existing["data"]["metadata"]["factorio_intake"] != item.id
        || existing["data"]["location"]["directory"] != w.config.repository
    {
        return Err(
            "OpenCode session ownership or directory changed; inspect it before resuming.".into(),
        );
    }
    let token = random();
    app.oauth
        .run("intake.key", |tx| {
            rp::lease(tx, &s.id, now())?;
            tx.insert(
                "factorio.intake_keys",
                snap_store::Row::from([
                    ("id".into(), rp::digest(&token).into()),
                    ("session".into(), s.id.clone().into()),
                    ("intake".into(), item.id.clone().into()),
                ]),
            )
        })
        .map_err(|e| format!("{e:?}"))?;
    // Store only the path in prompts. The scoped credential survives shell resets.
    let config = tool_config(&w, item);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(config.parent().unwrap())
        .map_err(|e| e.to_string())?;
    let temporary = config.with_extension(format!("{}.tmp", random()));
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(
            json!({"origin":app.oauth.config.origin,"token":token})
                .to_string()
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temporary, &config).map_err(|e| e.to_string())?;
    let cli = cli_path()?;
    // Retain compatibility for existing conversations; new prompts use the file.
    api(
        "PUT",
        &format!("{path}/environment"),
        Some(json!({"variables":{
            "FACTORIO_ORIGIN":app.oauth.config.origin,"FACTORIO_CLI":cli,
            "FACTORIO_INTAKE":item.id,"FACTORIO_INTAKE_TOKEN":token
        }})),
    )
    .await?;
    Ok(())
}
fn tool_config(w: &Workspace, item: &Intake) -> PathBuf {
    PathBuf::from(&w.config.resources)
        .join("intakes")
        .join(&item.id)
        .join("tool.json")
}
fn cli_path() -> Result<PathBuf, String> {
    let packaged_cli = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("factory.js");
    Ok(if packaged_cli.is_file() {
        packaged_cli
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("cli.ts")
    })
}
fn instructions(item: &Intake, w: &Workspace) -> Result<String, String> {
    let bun = std::env::var("FACTORIO_BUN").unwrap_or_else(|_| "bun".into());
    let executable = if bun.contains('/') {
        PathBuf::from(bun)
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(&bun))
            .find(|p| p.is_file())
            .ok_or("Bun executable not found")?
    }
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let quote = |p: &std::path::Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let command = format!("{} {}", quote(&executable), quote(&cli_path()?));
    let config = quote(&tool_config(w, item));
    let guide = include_str!("../../INTAKE.md")
        .replace(
            "bun \"$FACTORIO_CLI\" intake-read",
            &format!("{command} intake-read --intake-config {config}"),
        )
        .replace(
            "bun \"$FACTORIO_CLI\" intake-save -",
            &format!("{command} intake-save - --intake-config {config}"),
        );
    Ok(format!(
        "{}\n\nIntake ID: {}. Modules: {}.\n\nUser request:\n{}",
        guide,
        item.id,
        serde_json::to_string(&w.config.modules).unwrap(),
        item.description
    ))
}
pub async fn create(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let (s, _) = match app.actor(&headers, true).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    let (Some(id), Some(description)) = (input["id"].as_str(), input["description"].as_str())
    else {
        return failure(Error::Invalid);
    };
    let _gate = app.intake_gate.lock().await;
    let item = match app.oauth.run("intake.create", |tx| {
        factorio::intake::create(tx, actor(&s), id, description)
    }) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    match begin(&app, &s, &item).await {
        Ok(()) => no_store(json!(item)),
        Err(e) => error(e),
    }
}
async fn begin(app: &App, s: &rp::Session, item: &Intake) -> Result<(), String> {
    configure(app, s, item).await?;
    let w = app.workspace().map_err(|e| format!("{e:?}"))?;
    // Stable first-message ID makes retry after uncertain admission safe.
    api("POST", &format!("/api/session/{}/prompt",item.conversation), Some(json!({"id":format!("msg_{}_initial",item.id),"text":instructions(item,&w)?,"metadata":{"factorio_initial":true}}))).await?;
    Ok(())
}

pub async fn action(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let (s, human) = match app.actor(&headers, true).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let _gate = app.intake_gate.lock().await;
    let item = match owned(&app, &s, &id) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let path = format!("/api/session/{}", item.conversation);
    let result = match input["action"].as_str() {
        Some("delete") => {
            if let Err(e) = api("DELETE", &path, None).await {
                return error(e);
            }
            let result = app.oauth.run("intake.delete", |tx| {
                factorio::intake::delete(tx, actor(&s), &id)?;
                for row in tx.find("factorio.intake_keys", "primary", &[])? {
                    if row["intake"] == snap_store::Value::Text(id.clone()) {
                        tx.delete("factorio.intake_keys", &[row["id"].clone()])?;
                    }
                }
                Ok(())
            });
            if let Err(e) = result {
                return failure(e);
            }
            if let Ok(w) = app.workspace() {
                let _ = std::fs::remove_file(tool_config(&w, &item));
            }
            return no_store(json!({"deleted":id}));
        }
        Some("resume") => begin(&app, &s, &item).await.map(|_| json!(item)),
        Some("message") => {
            let (Some(text), Some(message)) = (input["text"].as_str(), input["id"].as_str()) else {
                return failure(Error::Invalid);
            };
            if text.trim().is_empty()
                || text.len() > 16384
                || !message.starts_with("msg_")
                || message.len() > 100
                || !message
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return failure(Error::Invalid);
            }
            match configure(&app, &s, &item).await {
                Ok(()) => {
                    api(
                        "POST",
                        &format!("{path}/prompt"),
                        Some(json!({"text":text,"id":message})),
                    )
                    .await
                }
                Err(e) => Err(e),
            }
        }
        Some("interrupt") => {
            api(
                "POST",
                &format!("{path}/interrupt"),
                Some(json!({"continue":false})),
            )
            .await
        }
        Some("form") | Some("permission") => {
            let Some(key) = input["id"].as_str() else {
                return failure(Error::Invalid);
            };
            if key.len() > 100 || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return failure(Error::Invalid);
            };
            if input["action"] == "form" {
                api(
                    "POST",
                    &format!("{path}/form/{key}/reply"),
                    Some(input["reply"].clone()),
                )
                .await
            } else {
                if !human || !matches!(input["reply"].as_str(), Some("once" | "reject")) {
                    return failure(Error::Invalid);
                };
                api(
                    "POST",
                    &format!("{path}/permission/{key}/reply"),
                    Some(json!({"reply":input["reply"]})),
                )
                .await
            }
        }
        Some("ready") => {
            let Some(revision) = input["revision"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
            else {
                return failure(Error::Invalid);
            };
            return match app.oauth.run("intake.ready", |tx| {
                factorio::intake::ready(tx, actor(&s), &id, revision)
            }) {
                Ok(v) => no_store(json!(v)),
                Err(e) => failure(e),
            };
        }
        _ => return failure(Error::Invalid),
    };
    match result {
        Ok(v) => no_store(v),
        Err(e) => error(e),
    }
}

pub async fn events(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (s, _) = match app.actor(&headers, false).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let item = match owned(&app, &s, &id) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let mut child =
        match process(json!({"watch":item.conversation,"description":item.description})).await {
            Ok(v) => v,
            Err(e) => return error(e),
        };
    let Some(output) = child.stdout.take() else {
        return error("Missing event stream");
    };
    let stream = async_stream::stream! {
        let _child=child; // kill-on-drop when browser disconnects
        let mut lines=BufReader::new(output).lines();
        let mut expiry=tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _=expiry.tick()=>{ if owned(&app,&s,&id).is_err() {yield Ok::<_,Infallible>(Event::default().event("expired").data("Sign in again"));break;} },
                line=lines.next_line()=>{
                    let Ok(Some(line))=line else {yield Ok(Event::default().event("unavailable").data("OpenCode disconnected; reconnecting"));break;};
                    if owned(&app,&s,&id).is_err() {break;}
                    if serde_json::from_str::<Value>(&line).is_err(){break;}
                    yield Ok(Event::default().event("snapshot").data(line));
                }
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Shell-tool endpoint: only the bound intake's drafts, never general commands.
pub async fn tool(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let Some(token) = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    else {
        return failure(Error::NotFound);
    };
    let result = app.oauth.run("intake.tool", |tx| {
        let row = tx
            .get("factorio.intake_keys", &[rp::digest(token).into()])?
            .ok_or(Error::NotFound)?;
        let (Some(snap_store::Value::Text(session)), Some(snap_store::Value::Text(id))) =
            (row.get("session"), row.get("intake"))
        else {
            return Err(Error::Invalid);
        };
        let who = rp::lease(tx, session, now())?;
        let w = factorio::load(tx)?;
        let item = w.intakes.get(id).ok_or(Error::NotFound)?;
        if item.owner != who.owner {
            return Err(Error::NotFound);
        };
        if input["action"] == "read" {
            return Ok(json!({"intake":item,"modules":w.config.modules,"tickets":w.tickets}));
        }
        let drafts: Drafts = serde_json::from_value(input).map_err(|_| Error::Invalid)?;
        factorio::intake::drafts(
            tx,
            Actor {
                session,
                human: false,
                now: now(),
            },
            id,
            drafts,
        )
        .map(|v| json!(v))
    });
    match result {
        Ok(v) => no_store(v),
        Err(e) => failure(e),
    }
}
