use crate::Service;
use axum::{
    Json, Router,
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use snap_transport::{Error, Event, Invocation, carrier::Dispatch};
use std::sync::Arc;

pub type WriteCookie = Arc<dyn Fn(Option<&str>) -> String + Send + Sync>;
pub struct HttpOperation {
    pub name: &'static str,
    pub method: Method,
    pub read_cookie: bool,
    pub write_cookie: WriteCookie,
}

pub fn http_router<D: Dispatch>(
    service: Arc<Service<D>>,
    operations: Vec<HttpOperation>,
) -> Router {
    let mut router = Router::new();
    for operation in operations {
        let path = format!("/{}", operation.name.replace('.', "/"));
        let service = service.clone();
        let method = operation.method.clone();
        let handler = move |headers: HeaderMap, body: axum::body::Bytes| {
            let service = service.clone();
            let method = method.clone();
            let write_cookie = operation.write_cookie.clone();
            async move {
                let id = headers
                    .get("x-snap-operation-id")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|id| *id > 0)
                    .unwrap_or(1);
                let forbidden = method == Method::POST
                    && headers.get("origin").and_then(|v| v.to_str().ok()) != Some(&service.origin);
                let reply: snap_transport::bearer::Reply = if forbidden {
                    Err(Error::Application(serde_json::json!({"code":"Forbidden"}))).into()
                } else {
                    let input = if method == Method::GET {
                        if body.is_empty() {
                            Ok(serde_json::Value::Null)
                        } else {
                            Err(Error::InvalidInput)
                        }
                    } else if headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.split(';').next())
                        != Some("application/json")
                    {
                        Err(Error::InvalidInput)
                    } else {
                        serde_json::from_slice(&body).map_err(|_| Error::InvalidInput)
                    };
                    match input {
                        Err(error) => Err(error).into(),
                        Ok(input) => {
                            let bearer = if operation.read_cookie {
                                service.cookie.as_ref().and_then(|read| read(&headers))
                            } else {
                                None
                            };
                            service
                                .dispatch
                                .request(
                                    Invocation {
                                        id,
                                        operation: operation.name.into(),
                                        input,
                                    },
                                    bearer,
                                )
                                .await
                        }
                    }
                };
                let cookie = if reply.outcome.is_ok() {
                    reply.bearer.as_ref().map(|change| match change {
                        snap_transport::bearer::Change::Set(token) => {
                            write_cookie(Some(token.expose()))
                        }
                        snap_transport::bearer::Change::Clear => write_cookie(None),
                    })
                } else {
                    None
                };
                let outcome = reply.outcome;
                let status = if forbidden {
                    StatusCode::FORBIDDEN
                } else {
                    match &outcome {
                        Ok(_) => StatusCode::OK,
                        Err(Error::InvalidInput) => StatusCode::BAD_REQUEST,
                        Err(Error::InvalidBearer | Error::IdentityRequired) => {
                            StatusCode::UNAUTHORIZED
                        }
                        Err(Error::UnknownOperation) => StatusCode::NOT_FOUND,
                        Err(Error::Application(value)) if value["code"] == "Conflict" => {
                            StatusCode::CONFLICT
                        }
                        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
                    }
                };
                let mut response = (
                    status,
                    [("cache-control", "no-store")],
                    Json(Event::Completed { id, outcome }),
                )
                    .into_response();
                if let Some(cookie) = cookie {
                    response
                        .headers_mut()
                        .insert("set-cookie", cookie.parse().expect("session cookie"));
                }
                response
            }
        };
        router = router.route(
            &path,
            if operation.method == Method::GET {
                get(handler)
            } else {
                post(handler)
            },
        );
    }
    router
}
