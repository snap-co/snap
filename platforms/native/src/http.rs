//! Native adapter for application-owned HTTP routes. Each route uses the same
//! local executor model as Protocol; HTTP clients never poll application futures.
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use snap_http::{Request, Service};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

struct Work {
    request: Request,
    reply: oneshot::Sender<snap_http::Response>,
    permit: OwnedSemaphorePermit,
}
#[derive(Clone)]
struct Host {
    inbox: mpsc::Sender<Work>,
    capacity: Arc<Semaphore>,
}

pub(crate) fn router(
    service: Box<dyn Service>,
    revoked: tokio::sync::broadcast::Sender<Vec<String>>,
) -> (Router, tokio::task::JoinHandle<()>) {
    let (inbox, mut receiver) = mpsc::channel::<Work>(64);
    let mut router = Router::new();
    for route in service.routes() {
        router = router.route(route, any(request));
    }
    let host = Host {
        inbox,
        capacity: Arc::new(Semaphore::new(64)),
    };
    let router = router
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(host);
    let worker = tokio::task::spawn_local(async move {
        let mut jobs = FuturesUnordered::new();
        loop {
            tokio::select! {
                work = receiver.recv() => {
                    let Some(work) = work else { break };
                    if work.reply.is_closed() { continue; }
                    let future = service.call(work.request);
                    let revoked=revoked.clone();
                    jobs.push(async move {
                        let _permit = work.permit;
                        let response=future.await;
                        if !response.terminate.is_empty() {let _=revoked.send(response.terminate.clone());}
                        let _ = work.reply.send(response);
                    });
                }
                _ = jobs.next(), if !jobs.is_empty() => {}
            }
        }
    });
    (router, worker)
}
async fn request(
    State(host): State<Host>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(permit) = host.capacity.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (reply, receive) = oneshot::channel();
    let request = Request {
        method: method.to_string(),
        path: uri.path().into(),
        query: uri.query().unwrap_or("").into(),
        headers: headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().into(), v.into())))
            .collect(),
        body: body.to_vec(),
        now: crate::now(),
    };
    if host
        .inbox
        .try_send(Work {
            request,
            reply,
            permit,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    // Long outbound generation calls may outlive this observer. Applications expose
    // persisted progress separately; disconnect is never interpreted as a retry.
    match tokio::time::timeout(Duration::from_secs(180), receive).await {
        Ok(Ok(reply)) => response(reply),
        _ => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}
fn response(reply: snap_http::Response) -> Response {
    let mut response = (
        StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        reply.body,
    )
        .into_response();
    for (key, value) in reply.headers {
        if let (Ok(key), Ok(value)) = (
            key.parse::<axum::http::HeaderName>(),
            value.parse::<axum::http::HeaderValue>(),
        ) {
            response.headers_mut().append(key, value);
        }
    }
    response
}
