//! Identity-authenticated OpenCode proxy. Application writes use WebSocket
//! operations; agent turns and event streams never hold the application gate.
use super::*;
use axum::{
    extract::Path,
    response::sse::{Event, KeepAlive, Sse},
};
use factorio::intake::Intake;
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

async fn process(app: &App, mut input: Value) -> Result<tokio::process::Child, String> {
    input["opencode"] = app.tools.opencode.clone().into();
    let mut child = Command::new(&app.tools.bun)
        .env_remove("SNAP_MASTER_KEY")
        .arg(app.tools.bridge.as_ref().ok_or("Missing tools.bridge")?)
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
async fn api(app: &App, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(40),
        process(app, json!({"method":method,"path":path,"body":body}))
            .await?
            .wait_with_output(),
    )
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
fn owned(
    app: &App,
    s: &rp::Grant,
    workspace: &str,
    id: &str,
) -> Result<(Workspace, Intake), Error> {
    app.oauth.run("intake.proxy-authority", |tx| {
        let who = rp::lease(tx, &s.id, now())?;
        graph::require_owner(tx, workspace, &who.owner)?;
        let w = graph::load(tx, workspace, &who.owner)?;
        let item = w.intakes.get(id).ok_or(Error::NotFound)?.clone();
        Ok((w, item))
    })
}
fn cli_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}
fn credentials(w: &Workspace, owner: &str) -> PathBuf {
    PathBuf::from(&w.config.resources)
        .join("clients")
        .join(rp::digest(owner))
        .join("credentials.json")
}
async fn configure(
    app: &App,
    s: &rp::Grant,
    workspace: &str,
    w: &Workspace,
    item: &Intake,
    token: &str,
) -> Result<(), String> {
    // The file holds a normal account agent credential, not intake-only authority.
    app.oauth
        .run("intake.proxy-credential", |tx| {
            let (agent, human) = operations::session(tx, token)?;
            if human || agent.owner != s.owner || agent.id != s.id {
                return Err(Error::NotFound);
            }
            Ok(())
        })
        .map_err(|_| "Invalid agent credential")?;
    let config = credentials(w, &s.owner);
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
            json!({"addr":app.tcp.to_string(),"ca_file":app.tcp_ca_file,"server_name":app.tcp_server_name,"token":token,"workspace":workspace,"client_id":random(),"next_id":1})
                .to_string()
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    std::fs::rename(temporary, config).map_err(|e| e.to_string())?;
    let path = format!("/api/session/{}", item.conversation);
    let model = &app.tools.model;
    let (provider, id) = model
        .split_once('/')
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
        .ok_or("tools.model must be provider/model")?;
    let model = json!({"providerID":provider,"id":id});
    let existing = match api(app, "GET",&path,None).await {
        Ok(v) => v,
        Err(_) => api(app, "POST","/api/session",Some(json!({"id":item.conversation,"model":model,"title":format!("Intake: {}",item.description.chars().take(70).collect::<String>()),"location":{"directory":w.config.repository},"metadata":{"factorio_intake":item.id,"factorio_workspace":workspace},"permissions":[{"action":"edit","resource":"*","effect":"deny"}]}))).await?,
    };
    if existing["data"]["metadata"]["factorio_intake"] != item.id
        || existing["data"]["metadata"]["factorio_workspace"] != workspace
        || existing["data"]["location"]["directory"] != w.config.repository
    {
        return Err("OpenCode session ownership or directory changed".into());
    }
    // Apply the policy to retained conversations before submitting another turn.
    if existing["data"]["model"] != model {
        api(
            app,
            "POST",
            &format!("{path}/model"),
            Some(json!({"model":model})),
        )
        .await?;
    }
    Ok(())
}
fn instructions(_app: &App, item: &Intake, w: &Workspace, owner: &str) -> Result<String, String> {
    let quote = |p: &std::path::Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let command = quote(
        &cli_path()?
            .canonicalize()
            .map_err(|_| "Factorio executable is unavailable")?,
    );
    let args = format!(
        "--credentials {} --intake {}",
        quote(&credentials(w, owner)),
        item.id
    );
    let guide = include_str!("../../prompts/intake.md")
        .replace(
            "factory intake-read",
            &format!("{command} intake-read {args}"),
        )
        .replace(
            "factory intake-save -",
            &format!("{command} intake-save - {args}"),
        );
    Ok(format!(
        "{guide}\n\nIntake ID: {}. Modules: {}.\n\nUser request:\n{}",
        item.id,
        serde_json::to_string(&w.config.modules).unwrap(),
        item.description
    ))
}
pub async fn action(
    State(app): State<Arc<App>>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let (s, human) = match app.actor(&headers, true).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let (w, item) = match owned(&app, &s, &workspace, &id) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let path = format!("/api/session/{}", item.conversation);
    let result = match input["action"].as_str() {
        Some("resume") | Some("message") => {
            let Some(token) = input["credential"].as_str() else {
                return failure(Error::Invalid);
            };
            if let Err(e) = configure(&app, &s, &workspace, &w, &item, token).await {
                return error(e);
            }
            if input["action"] == "resume" {
                let text = match instructions(&app, &item, &w, &s.owner) {
                    Ok(v) => v,
                    Err(e) => return error(e),
                };
                api(&app, "POST",&format!("{path}/prompt"),Some(json!({"id":format!("msg_{}_initial",item.id),"text":text,"metadata":{"factorio_initial":true}}))).await
            } else {
                let (Some(text), Some(message)) = (input["text"].as_str(), input["id"].as_str())
                else {
                    return failure(Error::Invalid);
                };
                if text.trim().is_empty()
                    || text.len() > 16384
                    || !message.starts_with("msg_")
                    || !safe_id(message)
                {
                    return failure(Error::Invalid);
                }
                api(
                    &app,
                    "POST",
                    &format!("{path}/prompt"),
                    Some(json!({"id":message,"text":text})),
                )
                .await
            }
        }
        Some("delete") => api(&app, "DELETE", &path, None).await,
        Some("interrupt") => {
            api(
                &app,
                "POST",
                &format!("{path}/interrupt"),
                Some(json!({"continue":false})),
            )
            .await
        }
        Some("form") | Some("permission") => {
            let Some(key) = input["id"].as_str().filter(|key| safe_id(key)) else {
                return failure(Error::Invalid);
            };
            if input["action"] == "form" {
                api(
                    &app,
                    "POST",
                    &format!("{path}/form/{key}/reply"),
                    Some(input["reply"].clone()),
                )
                .await
            } else {
                if !human || !matches!(input["reply"].as_str(), Some("once" | "reject")) {
                    return failure(Error::Invalid);
                }
                api(
                    &app,
                    "POST",
                    &format!("{path}/permission/{key}/reply"),
                    Some(json!({"reply":input["reply"]})),
                )
                .await
            }
        }
        _ => return failure(Error::Invalid),
    };
    match result {
        Ok(v) => no_store(v),
        Err(e) => error(e),
    }
}
fn safe_id(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 100
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub async fn events(
    State(app): State<Arc<App>>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let (s, _) = match app.actor(&headers, false).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let (_, item) = match owned(&app, &s, &workspace, &id) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let mut child = match process(
        &app,
        json!({"watch":item.conversation,"description":item.description}),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let Some(output) = child.stdout.take() else {
        return error("Missing event stream");
    };
    let stream = async_stream::stream! {
        let _child=child;
        let mut lines=BufReader::new(output).lines();
        let mut expiry=tokio::time::interval(Duration::from_secs(10));
        loop { tokio::select! {
            _=expiry.tick()=>{ if owned(&app,&s,&workspace,&id).is_err() { yield Ok::<_,Infallible>(Event::default().event("expired").data("Sign in again")); break; } },
            line=lines.next_line()=>{
                let Ok(Some(line))=line else { yield Ok(Event::default().event("unavailable").data("OpenCode disconnected; reconnecting")); break; };
                if owned(&app,&s,&workspace,&id).is_err() || serde_json::from_str::<Value>(&line).is_err() { break; }
                yield Ok(Event::default().event("snapshot").data(line));
            }
        }}
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
