//! Controlled wire peer. Contract assertions remain at the client SDK interface.
use axum::{Router, extract::State, http::HeaderMap, response::IntoResponse, routing::get};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

pub struct Peer {
    pub url: String,
    pub mode: Arc<AtomicU8>,
    pub arrived: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Peer {
    pub async fn start(mode: u8) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mode = Arc::new(AtomicU8::new(mode));
        let arrived = Arc::new(Notify::new());
        let router = Router::new()
            .route("/health/up", get(reply))
            .with_state((mode.clone(), arrived.clone()));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            mode,
            arrived,
            task,
        }
    }
    pub fn mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn reply(
    State((mode, arrived)): State<(Arc<AtomicU8>, Arc<Notify>)>,
    headers: HeaderMap,
) -> axum::response::Response {
    let mode = mode.load(Ordering::SeqCst);
    arrived.notify_one();
    if mode == 3 {
        std::future::pending::<()>().await;
    }
    let id = if mode == 2 {
        "wrong-operation"
    } else {
        headers["x-snap-operation-id"].to_str().unwrap()
    };
    if mode == 4 {
        return "not JSON".into_response();
    }
    let payload = if mode == 1 {
        serde_json::json!({"ok": false, "error": {"_tag": "UnavailableError", "message": "Service unavailable"}})
    } else {
        serde_json::json!({"ok": true, "payload": {"status": "OK"}})
    };
    axum::Json(serde_json::json!({"key": "transport.complete", "target": id, "payload": payload}))
        .into_response()
}
