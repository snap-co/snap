//! Local browser host. HTTP controls and WebSockets drive the same Development
//! instance. The listener must be loopback: these controls expose resident data.
mod carrier;
mod debugger;
#[cfg(test)]
mod tests;
use crate::development::{Control, Development};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use snap_transport::execution::Program;
use snap_transport::{json, server::Authority};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tower_http::services::{ServeDir, ServeFile};

struct Shared<P: Program, R: Authority> {
    host: Mutex<Development<P, R>>,
    clock: Instant,
    authority: String,
    updates: watch::Sender<debugger::Report>,
}
type Host<P, R> = Arc<Shared<P, R>>;

impl<P: Program, R: Authority> Shared<P, R> {
    /// Publish under the host lock so concurrent controls cannot reorder reports.
    /// The watch retains only the latest full report, never a per-observer queue.
    fn publish(&self, host: &Development<P, R>) {
        self.updates
            .send_if_modified(|report| report.update(host.inspect()));
    }
    fn change<T>(&self, change: impl FnOnce(&mut Development<P, R>) -> T) -> T {
        let mut host = self.host.lock().unwrap();
        let result = change(&mut host);
        self.publish(&host);
        result
    }
}

// Reject cross-origin browser control requests and DNS-rebinding Host headers.
fn local(headers: &HeaderMap, authority: &str) -> bool {
    let host = authority
        .strip_prefix("http://")
        .or_else(|| authority.strip_prefix("https://"))
        .unwrap_or(authority);
    let origin = if authority.contains("://") {
        authority.to_owned()
    } else {
        format!("http://{authority}")
    };
    headers.get("host").and_then(|h| h.to_str().ok()) == Some(host)
        && headers
            .get("origin")
            .is_none_or(|header| header.to_str().ok() == Some(&origin))
}
async fn inspect<P: Program + Send + 'static, R: Authority + Send + 'static>(
    State(shared): State<Host<P, R>>,
    headers: HeaderMap,
) -> Response {
    if !local(&headers, &shared.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match tokio::task::spawn_blocking(move || shared.host.lock().unwrap().inspect()).await {
        Ok(report) => Json(report).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn control<P: Program + Send + 'static, R: Authority + Send + 'static>(
    State(shared): State<Host<P, R>>,
    headers: HeaderMap,
    Json(control): Json<Control>,
) -> Response {
    if !local(&headers, &shared.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        shared.change(|host| host.control(control, shared.clock.elapsed().as_millis() as u64))
    })
    .await
    .unwrap_or_else(|_| Err("host control unavailable".into()));
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error": error}))).into_response(),
    }
}
pub async fn serve<P: Program + Send + 'static, R: Authority + Send + 'static>(
    listener: tokio::net::TcpListener,
    host: Development<P, R>,
    assets: String,
) -> std::io::Result<()> {
    let origin = format!("http://{}", listener.local_addr()?);
    serve_configured(listener, host, assets, origin, true).await
}

/// Production serves transport without publishing trusted debugger controls.
pub async fn serve_configured<P: Program + Send + 'static, R: Authority + Send + 'static>(
    listener: tokio::net::TcpListener,
    host: Development<P, R>,
    assets: String,
    origin: String,
    development: bool,
) -> std::io::Result<()> {
    let address = listener.local_addr()?;
    if development && !address.ip().is_loopback() {
        return Err(std::io::Error::other(
            "development controls require a loopback listener",
        ));
    }
    let (updates, _) = watch::channel(debugger::Report::new(host.inspect()));
    let shared = Arc::new(Shared {
        host: Mutex::new(host),
        clock: Instant::now(),
        authority: origin,
        updates,
    });
    let mut app = Router::new().fallback_service(
        ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
    );
    if development {
        app = app
            .route("/__dev", get(inspect::<P, R>).post(control::<P, R>))
            .route("/__dev/ws", get(debugger::upgrade::<P, R>));
    } else {
        app = app
            .route(
                "/__dev",
                axum::routing::any(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/__dev/{*path}",
                axum::routing::any(|| async { StatusCode::NOT_FOUND }),
            );
    }
    let transport =
        snap_transport::native::web::router(Arc::new(snap_transport::native::web::Service {
            dispatch: carrier::Dispatcher(shared.clone()),
            origin: shared.authority.clone(),
            cookie: None,
            require_cookie: false,
        }));
    let app = app.with_state(shared.clone()).merge(transport);
    let sweep = async {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            let shared = shared.clone();
            tokio::task::spawn_blocking(move || {
                let mut host = shared.host.lock().unwrap();
                if host.tick(shared.clock.elapsed().as_millis() as u64) {
                    shared.publish(&host);
                }
            })
            .await
            .expect("development sweep panicked");
        }
    };
    tokio::select! { result = axum::serve(listener, app) => result, _ = sweep => unreachable!() }
}
