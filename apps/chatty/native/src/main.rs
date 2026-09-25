use chatty::{FileRequest, Host};
use snap_http::{
    FutureValue, Response,
    client::{Client, Incoming, Outgoing},
};
use snap_protocol::{Invocation, Operation, Provider};
use snap_store::{Predicate, Query, Statement, Transaction};
use std::{io, path::PathBuf};

#[derive(Clone)]
struct Native {
    http: snap_native::outgoing::Http,
    files: snap_native::files::Files,
}
impl Client for Native {
    type Body = <snap_native::outgoing::Http as Client>::Body;
    async fn send(&self, req: Outgoing) -> Result<Incoming<Self::Body>, String> {
        self.http.send(req).await
    }
}
impl Host for Native {
    fn now(&self) -> u64 {
        snap_native::now()
    }
    fn random(&self) -> Result<String, Response> {
        Ok(snap_native::oidc::random())
    }
    async fn verify(
        &self,
        token: String,
        jwks: serde_json::Value,
    ) -> Result<serde_json::Value, Response> {
        snap_native::oidc::verify(token, jwks).await
    }
    fn spawn(&self, future: FutureValue<()>) {
        tokio::task::spawn_local(future);
    }
    async fn sleep(&self, milliseconds: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(milliseconds)).await;
    }
    async fn files(
        &self,
        owner: String,
        request: FileRequest,
    ) -> Result<serde_json::Value, String> {
        use snap_native::files::Request;
        self.files
            .execute(
                owner,
                match request {
                    FileRequest::List => Request::List,
                    FileRequest::Read { path } => Request::Read(path),
                    FileRequest::Write { path, content } => Request::Write(path, content),
                },
            )
            .await
    }
}
struct Empty;
impl Provider for Empty {
    type Context = Option<String>;
    type Output = snap_native::Reply;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        core::iter::empty()
    }
    // Provider requires an owned 'static future; async fn would borrow self.
    #[allow(clippy::manual_async_fn)]
    fn invoke(
        &mut self,
        _: Invocation,
        _: Option<String>,
    ) -> impl core::future::Future<Output = Self::Output> + 'static {
        async { unreachable!("no published Snap operations") }
    }
}
fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}
fn origin(key: &str, default: &str) -> io::Result<String> {
    Ok(snap_native::parse_origin(&env(key, default))?
        .origin()
        .ascii_serialization())
}
fn main() -> io::Result<()> {
    let config = snap_native::Config::from_env("chatty")?;
    let origin = origin("SNAP_ORIGIN", &format!("http://{}", config.address))?;
    let issuer = origin_value("AUTHY_ORIGIN", "http://127.0.0.1:3846")?;
    let store = snap_native::store::Store::sqlite(
        &PathBuf::from(env("SNAP_DATABASE", ".snap/chatty.sqlite")),
        &chatty::schemas(),
    )
    .map_err(|_| io::Error::other("Cannot initialize Chatty database"))?;
    let key_query = Query::new(chatty::storage::SETTINGS)
        .matching(vec![Predicate::eq("key", "cookie")])
        .limit(1);
    let mut rows = store
        .execute(Transaction {
            guards: vec![],
            statements: vec![Statement::Select(key_query)],
        })
        .map_err(|_| io::Error::other("Cannot load Chatty cookie key"))?;
    let key = if let Some(record) = rows[0].pop() {
        chatty::storage::text(&record, "value")
            .map_err(|_| io::Error::other("Invalid cookie key"))?
    } else {
        let key = snap_native::oidc::random();
        store
            .execute(Transaction {
                guards: vec![],
                statements: vec![Statement::Insert {
                    table: chatty::storage::SETTINGS,
                    row: chatty::storage::row(&[
                        ("key", "cookie".into()),
                        ("value", key.clone().into()),
                    ]),
                }],
            })
            .map_err(|_| io::Error::other("Cannot persist cookie key"))?;
        key
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime
        .block_on(chatty::storage::recover(&store, snap_native::now()))
        .map_err(|_| io::Error::other("Cannot recover interrupted Chatty work"))?;
    drop(runtime);
    let secure = origin.starts_with("https:");
    let cookie = snap_native::cookie::Cookie::new(
        key.as_bytes().to_vec(),
        "chatty",
        secure,
        30 * 24 * 3600,
    )?;
    let correlation =
        snap_native::cookie::Cookie::new(key.into_bytes(), "chatty_login", secure, 300)?;
    let host = Native {
        http: snap_native::outgoing::Http::new().map_err(io::Error::other)?,
        files: snap_native::files::Files::new(PathBuf::from(env(
            "CHATTY_FILES",
            ".snap/chatty-files",
        )))?,
    };
    let app = chatty::Config {
        origin,
        issuer,
        client_id: "chatty".into(),
        client_secret: env("CHATTY_CLIENT_SECRET", ""),
        model: snap_llm::Config {
            endpoint: env(
                "CHATTY_MODEL_ENDPOINT",
                "https://opencode.ai/zen/go/v1/responses",
            ),
            model: env("CHATTY_MODEL", "muse-spark-1.3-contributor"),
            key: env("OPENCODE_API_KEY", ""),
            max_output_tokens: 8192,
        },
        exa_key: env("EXA_API_KEY", ""),
        files: true,
    };
    snap_native::run_application_with_http(
        Empty,
        config,
        snap_native::Web {
            bindings: vec![],
            session: None,
        },
        chatty::web::Web::new(store, host, app, cookie, correlation),
    )
}
fn origin_value(key: &str, default: &str) -> io::Result<String> {
    origin(key, default)
}
