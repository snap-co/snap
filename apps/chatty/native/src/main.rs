mod tools;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chatty::{Job, Outcome, Progress, Send};
use serde_json::{Value, json};
use snap_document_local::{Host, web::Shared};
use snap_oauth_local::{Config, Cookies, OAuth, failure, no_store, now, random};
use snap_oidc::relying_party as rp;
use snap_store::Error;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tower_http::services::{ServeDir, ServeFile};

struct App {
    oauth: Arc<OAuth>,
    model: snap_model_local::Config,
    http: snap_model_local::Http,
    files: tools::Files,
    exa_key: String,
    permits: Arc<tokio::sync::Semaphore>,
}
struct LiveProgress {
    value: Progress,
    last_flush: Instant,
    dirty: usize,
}
fn field<'a>(input: &'a Value, key: &str) -> Result<&'a str, Error> {
    input[key].as_str().ok_or(Error::Invalid)
}
async fn session(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.oauth.session(&headers).await {
        Ok(session) => no_store(
            json!({"identified":true,"csrf":session.csrf,"account":{"id":session.subject,"owner":session.owner,"name":session.profile["name"],"email":session.profile["email"]},"model":app.model.model,"model_ready":!app.model.key.is_empty(),"files_available":true,"search_available":!app.exa_key.is_empty()}),
        ),
        Err(Error::NotFound) => no_store(json!({"identified":false})),
        Err(error) => failure(error),
    }
}
async fn command(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Json(input): Json<Value>,
) -> Response {
    let session = match app.oauth.session(&headers).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if app.oauth.csrf(&headers, &session).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let result = match uri.path() {
        "/api/thread/create" => {
            let id = uuid::Uuid::new_v4().to_string();
            app.oauth
                .run("chatty.create", |tx| {
                    chatty::create(
                        tx,
                        &session.id,
                        &id,
                        input["title"].as_str().unwrap_or("New thread"),
                        input["effort"].as_str().unwrap_or("medium"),
                        now(),
                    )
                })
                .map(|_| json!({"id":id}))
        }
        "/api/thread/delete" => app
            .oauth
            .run("chatty.delete", |tx| {
                chatty::remove(tx, &session.id, field(&input, "thread_id")?, now())
            })
            .map(|_| json!({"deleted":true})),
        "/api/cancel" => app
            .oauth
            .run("chatty.cancel", |tx| {
                chatty::cancel(
                    tx,
                    &session.id,
                    field(&input, "thread_id")?,
                    field(&input, "turn_id")?,
                    now(),
                )
            })
            .map(|_| json!({"cancelled":true})),
        "/api/send" => {
            if app.model.key.is_empty() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error_description":"OPENCODE_API_KEY is not configured"})),
                )
                    .into_response();
            }
            let permit = app.permits.clone().try_acquire_owned().ok();
            let accepted = app.oauth.run("chatty.accept", |tx| {
                let accepted = chatty::send(
                    tx,
                    Send {
                        session: &session.id,
                        thread: field(&input, "thread_id")?,
                        turn: &random(),
                        request: field(&input, "request_id")?,
                        message: field(&input, "message")?,
                        now: now(),
                    },
                )?;
                if accepted.job.is_some() && permit.is_none() {
                    return Err(Error::Constraint);
                }
                Ok(accepted)
            });
            match accepted {
                Ok(accepted) => {
                    if let Some(job) = accepted.job {
                        let app = app.clone();
                        let permit = permit.expect("reserved before acceptance");
                        tokio::spawn(async move {
                            let _permit = permit;
                            app.generate(Arc::new(job)).await;
                        });
                    }
                    return (
                        StatusCode::ACCEPTED,
                        [("cache-control", "no-store")],
                        Json(json!({"turn_id":accepted.turn,"accepted":true})),
                    )
                        .into_response();
                }
                Err(e) => Err(e),
            }
        }
        _ => Err(Error::Invalid),
    };
    match result {
        Ok(value) => no_store(value),
        Err(e) => failure(e),
    }
}
impl App {
    fn publish(&self, job: &Job, p: &Progress, outcome: Outcome<'_>) -> Result<(), String> {
        self.oauth
            .run("chatty.progress", |tx| {
                chatty::progress(tx, job, p, outcome, now())
            })
            .map_err(|_| "Reply was stopped or the session ended".into())
    }
    async fn generate(self: Arc<Self>, job: Arc<Job>) {
        let p = Arc::new(Mutex::new(LiveProgress {
            value: Progress {
                usage: json!({"context_omitted":job.omitted}),
                ..Default::default()
            },
            last_flush: Instant::now(),
            dirty: 0,
        }));
        let result = self.steps(job.clone(), p.clone()).await;
        let snapshot = p.lock().unwrap().value.clone();
        let outcome = match &result {
            Ok(()) => Outcome::Complete,
            Err(error) => Outcome::Failed(error),
        };
        if self.publish(&job, &snapshot, outcome).is_err() {
            let _ = self
                .oauth
                .run("chatty.abandon", |tx| chatty::abandon(tx, &job, now()));
        }
    }
    async fn steps(
        self: &Arc<Self>,
        job: Arc<Job>,
        p: Arc<Mutex<LiveProgress>>,
    ) -> Result<(), String> {
        let mut input = job.input.clone();
        let mut calls = 0;
        for _ in 0..5 {
            self.oauth
                .run("chatty.effect.authority", |tx| {
                    chatty::can_continue(tx, &job, now())
                })
                .map_err(|_| "Reply was stopped")?;
            if serde_json::to_vec(&input)
                .map_err(|_| "Invalid context")?
                .len()
                > 320 * 1024
            {
                return Err("This reply exceeded the context budget".into());
            }
            let (prefix_text, prefix_summary) = {
                let p = p.lock().unwrap();
                (p.value.text.clone(), p.value.summary.clone())
            };
            let app = self.clone();
            let observed = p.clone();
            let job_copy = job.clone();
            let result = snap_model_local::generate(
                &self.http,
                &self.model,
                input.clone(),
                &job.thread,
                &job.effort,
                tools::definitions(!self.exa_key.is_empty()),
                move |event| {
                    let app = app.clone();
                    let p = observed.clone();
                    let job = job_copy.clone();
                    Box::pin(async move {
                        let snapshot = {
                            let mut p = p.lock().unwrap();
                            match event {
                                snap_model_local::Event::Text(s) => {
                                    p.dirty += s.len();
                                    p.value.text.push_str(&s);
                                }
                                snap_model_local::Event::Summary(s) => {
                                    p.dirty += s.len();
                                    p.value.summary.push_str(&s);
                                }
                            }
                            if p.value.text.len() + p.value.summary.len() > 512 * 1024 {
                                return Err("Reply exceeded its display limit".into());
                            }
                            if p.dirty >= 1024
                                || p.last_flush.elapsed() >= Duration::from_millis(250)
                            {
                                p.dirty = 0;
                                p.last_flush = Instant::now();
                                Some(p.value.clone())
                            } else {
                                None
                            }
                        };
                        if let Some(snapshot) = snapshot {
                            app.publish(&job, &snapshot, Outcome::Progress)?;
                        }
                        Ok(())
                    })
                },
            )
            .await?;
            {
                let mut p = p.lock().unwrap();
                p.value.text = format!("{prefix_text}{}", result.text);
                p.value.summary = format!("{prefix_summary}{}", result.summary);
                p.value.output.extend(result.output.clone());
                add_usage(&mut p.value.usage, &result.usage);
            }
            self.publish(&job, &p.lock().unwrap().value, Outcome::Progress)?;
            if !result.complete {
                return Err(
                    "The model reached its output limit. Send a follow-up to continue.".into(),
                );
            }
            input.extend(snap_model_local::replay(&result.output));
            let functions: Vec<_> = result
                .output
                .iter()
                .filter(|v| v["type"] == "function_call")
                .collect();
            if functions.is_empty() {
                return Ok(());
            }
            for function in functions {
                calls += 1;
                if calls > 8 {
                    return Err("This reply reached its eight-tool-call limit".into());
                }
                let call = function["call_id"]
                    .as_str()
                    .ok_or("Model tool call has no ID")?;
                let name = function["name"]
                    .as_str()
                    .ok_or("Model tool call has no name")?;
                let args: Value = serde_json::from_str(
                    function["arguments"]
                        .as_str()
                        .ok_or("Missing tool arguments")?,
                )
                .map_err(|_| "Invalid tool arguments")?;
                {
                    let mut p = p.lock().unwrap();
                    p.value.tools.push(
                        json!({"call_id":call,"name":name,"arguments":args,"status":"running"}),
                    );
                }
                self.publish(&job, &p.lock().unwrap().value, Outcome::Progress)?;
                let value = match tools::execute(
                    &self.files,
                    &self.http,
                    &self.exa_key,
                    &job.owner,
                    name,
                    args,
                )
                .await
                {
                    Ok(value) => json!({"ok":true,"result":value}),
                    Err(error) => json!({"ok":false,"error":error}),
                };
                let output = json!({"type":"function_call_output","call_id":call,"output":value.to_string()});
                {
                    let mut p = p.lock().unwrap();
                    let tool = p.value.tools.last_mut().ok_or("Missing tool record")?;
                    tool["status"] = "complete".into();
                    tool["result"] = value;
                    p.value.output.push(output.clone());
                }
                input.push(output);
                self.publish(&job, &p.lock().unwrap().value, Outcome::Progress)?;
            }
        }
        Err("This reply reached its five-model-step limit".into())
    }
}
fn add_usage(total: &mut Value, step: &Value) {
    for (name, value) in [
        ("input_tokens", step["input_tokens"].as_u64().unwrap_or(0)),
        ("output_tokens", step["output_tokens"].as_u64().unwrap_or(0)),
        (
            "reasoning_tokens",
            step["output_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
        ),
        (
            "cached_tokens",
            step["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        ),
        ("model_steps", 1),
    ] {
        total[name] = total[name]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(value)
            .into();
    }
}
fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut values: Vec<_> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        rp::MIGRATION,
        snap_oauth_local::MIGRATION,
        chatty::MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    values.sort_by(|a: &snap_store::migration::Migration, b| a.id.cmp(&b.id));
    values
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = PathBuf::from(env("SNAP_DATABASE", ".snap/chatty-store.sqlite"));
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--migrate"] {
        snap_sqlite::migrate(&database, &migrations())?;
        println!("Chatty migrations applied");
        return Ok(());
    }
    if !args.is_empty() {
        return Err("usage: chatty [--migrate]".into());
    }
    let listener = tokio::net::TcpListener::bind(env("CHATTY_ADDR", "127.0.0.1:3850")).await?;
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err("Chatty development host requires loopback".into());
    }
    let origin = snap_oauth_local::origin(&env("SNAP_ORIGIN", &format!("http://{address}")))?;
    let issuer = env("AUTHY_ORIGIN", "http://127.0.0.1:3846");
    let mut store = snap_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(chatty::TABLES.iter())
    {
        store.load(table)?;
    }
    store.run("chatty.recover", |tx| chatty::recover(tx, now()))?;
    let cookies = Cookies::load(&mut store, "chatty", origin.starts_with("https:"))?;
    let host = Host::new(
        store,
        chatty::document(),
        Arc::new(|tx, bearer| rp::lease(tx, &rp::digest(bearer), now()).map(|s| s.owner)),
        snap_transport::server::Config::default(),
        random(),
    );
    let documents = Shared::with_cookie(host, origin.clone(), cookies.reader());
    let oauth = OAuth::new(
        documents.clone(),
        cookies,
        Config {
            origin,
            issuer,
            client: "chatty".into(),
            secret: env("CHATTY_CLIENT_SECRET", ""),
        },
    )?;
    let app = Arc::new(App {
        oauth: oauth.clone(),
        model: snap_model_local::Config {
            endpoint: env(
                "CHATTY_MODEL_ENDPOINT",
                "https://opencode.ai/zen/go/v1/responses",
            ),
            model: env("CHATTY_MODEL", "muse-spark-1.3-contributor"),
            key: env("OPENCODE_API_KEY", ""),
            max_output_tokens: 8192,
        },
        http: snap_model_local::Http::new().map_err(std::io::Error::other)?,
        files: tools::Files::new(PathBuf::from(env("CHATTY_FILES", ".snap/chatty-files")))?,
        exa_key: env("EXA_API_KEY", ""),
        permits: Arc::new(tokio::sync::Semaphore::new(4)),
    });
    let assets = std::env::var("SNAP_WEB_DIR").unwrap_or_else(|_| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("web")))
            .filter(|p| p.is_dir())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".snap/web".into())
    });
    let mut router = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/api/session", get(session));
    for path in [
        "/api/thread/create",
        "/api/thread/delete",
        "/api/send",
        "/api/cancel",
    ] {
        router = router.route(path, post(command));
    }
    let router = router
        .with_state(app)
        .merge(oauth.routes())
        .merge(snap_document_local::web::router(documents.clone()))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    println!("Chatty http://{address}");
    tokio::select! {result=axum::serve(listener,router).with_graceful_shutdown(async{let _=tokio::signal::ctrl_c().await;})=>result?,_=snap_document_local::web::dispatch(documents)=>unreachable!()}
    Ok(())
}
