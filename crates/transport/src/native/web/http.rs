use super::Service;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, RawQuery},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use snap_transport::{Error, Event, Invocation, Value, carrier::Dispatch};
use std::{collections::BTreeMap, sync::Arc};

pub type WriteCookie = Arc<dyn Fn(Option<&str>) -> String + Send + Sync>;
pub type Parameters = BTreeMap<String, String>;
type Decode = Arc<dyn Fn(&HeaderMap, Parameters) -> Result<Value, Error> + Send + Sync>;
type Encode = Arc<dyn Fn(&Value) -> Result<Response, Error> + Send + Sync>;
type Project = Arc<dyn Fn(&HeaderMap, Value) -> Result<Value, Error> + Send + Sync>;

/// Host-selected physical encoding of a declared operation. Adapters only decode
/// inputs and encode terminal results; they cannot execute or issue credentials.
pub struct HttpOperation {
    pub name: &'static str,
    pub method: Method,
    pub read_cookie: bool,
    pub write_cookie: WriteCookie,
    path: String,
    decode: Option<Decode>,
    encode: Option<Encode>,
    form_origin: Option<String>,
    project: Option<Project>,
}
impl HttpOperation {
    pub fn from_route(
        route: snap_transport::carrier::HttpRoute,
        write_cookie: WriteCookie,
    ) -> Self {
        use snap_transport::carrier::HttpMethod;
        Self {
            name: route.operation,
            method: match route.method {
                HttpMethod::Get => Method::GET,
                HttpMethod::Post => Method::POST,
            },
            read_cookie: route.read_bearer,
            write_cookie,
            path: format!("/{}", route.operation.replace('.', "/")),
            decode: None,
            encode: None,
            form_origin: None,
            project: None,
        }
    }
    pub fn for_operation<O: snap_transport::Operation>(write_cookie: WriteCookie) -> Self {
        Self::from_route(
            O::http_route().expect("operation declares HTTP"),
            write_cookie,
        )
    }
    pub fn at(mut self, path: &str) -> Self {
        self.path = path.into();
        self
    }
    /// Unique, strictly decoded query/form fields. Cookie injection and protocol
    /// projection belong here, not in the operation's portable invocation shape.
    pub fn parameters(
        mut self,
        path: &str,
        decode: impl Fn(&HeaderMap, Parameters) -> Result<Value, Error> + Send + Sync + 'static,
    ) -> Self {
        self.path = path.into();
        self.decode = Some(Arc::new(decode));
        self
    }
    /// Also accept form POST from this explicitly trusted origin. Other POSTs
    /// retain same-origin enforcement. Continuity proof is still operation-owned.
    pub fn form_post(mut self, origin: String) -> Self {
        assert!(self.decode.is_some(), "form POST needs a parameter decoder");
        self.form_origin = Some(origin);
        self
    }
    /// Project ordinary JSON or an empty body, with transport-owned headers.
    /// This does not bypass field decoding, origin checks or operation validation.
    pub fn input(
        mut self,
        project: impl Fn(&HeaderMap, Value) -> Result<Value, Error> + Send + Sync + 'static,
    ) -> Self {
        self.project = Some(Arc::new(project));
        self
    }
    /// Called only with the final successful result, after host completion and
    /// commit. Shared adaptation applies committed bearer changes afterwards.
    pub fn response(
        mut self,
        encode: impl Fn(&Value) -> Result<Response, Error> + Send + Sync + 'static,
    ) -> Self {
        self.encode = Some(Arc::new(encode));
        self
    }
}

pub fn redirect(target: &str) -> Result<Response, Error> {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(
        "location",
        target.parse().map_err(|_| Error::InvalidOutput)?,
    );
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    Ok(response)
}
pub fn cookie(response: &mut Response, value: String) -> Result<(), Error> {
    response.headers_mut().append(
        "set-cookie",
        value.parse().map_err(|_| Error::InvalidOutput)?,
    );
    Ok(())
}
fn fields(raw: &[u8]) -> Result<Parameters, Error> {
    if raw.len() > 32 * 1024 {
        return Err(Error::InvalidInput);
    }
    let raw = std::str::from_utf8(raw).map_err(|_| Error::InvalidInput)?;
    let decode = |value: &str| {
        let bytes = value.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'%'
                && !bytes
                    .get(i + 1..i + 3)
                    .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
            {
                return Err(Error::InvalidInput);
            }
        }
        percent_encoding::percent_decode_str(&value.replace('+', " "))
            .decode_utf8()
            .map(|s| s.into_owned())
            .map_err(|_| Error::InvalidInput)
    };
    let mut fields = Parameters::new();
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if fields.insert(decode(key)?, decode(value)?).is_some() {
            return Err(Error::InvalidInput);
        }
    }
    Ok(fields)
}
fn content_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("content-type")?
        .to_str()
        .ok()?
        .split(';')
        .next()
        .map(str::trim)
}
fn failure(id: u64, error: Error, forbidden: bool) -> Response {
    let status = if forbidden {
        StatusCode::FORBIDDEN
    } else {
        match &error {
            Error::InvalidInput => StatusCode::BAD_REQUEST,
            Error::InvalidBearer | Error::IdentityRequired => StatusCode::UNAUTHORIZED,
            Error::UnknownOperation => StatusCode::NOT_FOUND,
            Error::Application(value) if value["code"] == "Conflict" => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        }
    };
    (
        status,
        [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
        ],
        Json(Event::Completed {
            id,
            outcome: Err(error),
        }),
    )
        .into_response()
}

pub fn http_router<D: Dispatch>(
    service: Arc<Service<D>>,
    operations: Vec<HttpOperation>,
) -> Router {
    let mut router = Router::new();
    for operation in operations {
        let path = operation.path.clone();
        let get_method = operation.method == Method::GET;
        let form_post = operation.form_origin.is_some();
        let operation = Arc::new(operation);
        let service = service.clone();
        let handler = move |method: Method,
                            headers: HeaderMap,
                            RawQuery(query): RawQuery,
                            body: axum::body::Bytes| {
            let service = service.clone();
            let operation = operation.clone();
            async move {
                let id = headers
                    .get("x-snap-operation-id")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|id| *id > 0)
                    .unwrap_or(1);
                let expected = operation.form_origin.as_deref().unwrap_or(&service.origin);
                let forbidden = method == Method::POST
                    && headers.get("origin").and_then(|v| v.to_str().ok()) != Some(expected);
                if forbidden {
                    return failure(
                        id,
                        Error::Application(serde_json::json!({"code":"Forbidden"})),
                        true,
                    );
                }
                let input = (|| {
                    if let Some(decode) = &operation.decode {
                        let params = if method == Method::GET {
                            if !body.is_empty() {
                                return Err(Error::InvalidInput);
                            }
                            fields(query.as_deref().unwrap_or("").as_bytes())?
                        } else {
                            // Never merge query and body with an ambiguous precedence.
                            if query.as_ref().is_some_and(|q| !q.is_empty())
                                || content_type(&headers)
                                    != Some("application/x-www-form-urlencoded")
                            {
                                return Err(Error::InvalidInput);
                            }
                            fields(&body)?
                        };
                        decode(&headers, params)
                    } else if method == Method::GET {
                        if body.is_empty() && query.as_ref().is_none_or(String::is_empty) {
                            Ok(Value::Null)
                        } else {
                            Err(Error::InvalidInput)
                        }
                    } else if query.as_ref().is_some_and(|q| !q.is_empty()) {
                        Err(Error::InvalidInput)
                    } else if operation.project.is_some() && body.is_empty() {
                        Ok(Value::Null)
                    } else if content_type(&headers) == Some("application/json") {
                        serde_json::from_slice(&body).map_err(|_| Error::InvalidInput)
                    } else {
                        Err(Error::InvalidInput)
                    }
                })();
                let input = input.and_then(|input| match &operation.project {
                    Some(project) => project(&headers, input),
                    None => Ok(input),
                });
                let input = match input {
                    Ok(input) => input,
                    Err(error) => return failure(id, error, false),
                };
                let bearer = operation
                    .read_cookie
                    .then(|| (service.cookie)(&headers))
                    .flatten();
                let reply = service
                    .dispatch
                    .request(
                        Invocation {
                            id,
                            operation: operation.name.into(),
                            input,
                        },
                        bearer,
                    )
                    .await;
                let output = match reply.outcome {
                    Ok(output) => output,
                    Err(error) => return failure(id, error, false),
                };
                let response = (|| {
                    let mut response = if let Some(encode) = &operation.encode {
                        encode(&output)?
                    } else {
                        (
                            [("cache-control", "no-store")],
                            Json(Event::Completed {
                                id,
                                outcome: Ok(output),
                            }),
                        )
                            .into_response()
                    };
                    if let Some(change) = reply.bearer {
                        cookie(
                            &mut response,
                            match change {
                                snap_transport::bearer::Change::Set(token) => {
                                    (operation.write_cookie)(Some(token.expose()))
                                }
                                snap_transport::bearer::Change::Clear => {
                                    (operation.write_cookie)(None)
                                }
                            },
                        )?;
                    }
                    Ok(response)
                })();
                response.unwrap_or_else(|error| failure(id, error, false))
            }
        };
        let route = if get_method {
            if form_post {
                get(handler.clone()).post(handler)
            } else {
                get(handler)
            }
        } else {
            post(handler)
        };
        router = router.route(&path, route);
    }
    router.layer(DefaultBodyLimit::max(64 * 1024))
}
