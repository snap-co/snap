//! HTTP encoding and persisted cookie contracts, through real host execution.
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use snap_transport::native::{
    Server, WebSocket,
    web::{Cookies, HttpOperation, cookie, redirect},
};
use snap_transport::{
    Operation, Value,
    bearer::{Change, Receiver, Token},
    operation::{Definition, Registry},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

struct Exchange;
impl Operation for Exchange {
    const NAME: &'static str = "fixture.exchange";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Get, false));
    type Input = Value;
    type Output = Value;
    type Error = ();
    type Progress = ();
}

#[tokio::test]
async fn parameters_dispatch_once_and_only_final_committed_bearers_become_cookies() {
    let migrations =
        [snap_store::resource::MIGRATION, Cookies::MIGRATION].map(|s| toml::from_str(s).unwrap());
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
    let cookies = Cookies::load(&mut store, "fixture", false).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let definition = Definition::staged::<Exchange, Value>(
        false,
        vec![],
        Default::default(),
        &[],
        move |_, input, context| {
            count.fetch_add(1, Ordering::SeqCst);
            context
                .bearer_changed(Change::Set(Token::new("uncompleted".into())))
                .unwrap();
            Ok(input)
        },
        |_, input, context| {
            if input["mode"] == "failure" {
                return Err(snap_store::Error::Unavailable.into());
            }
            context
                .bearer_changed(Change::Set(Token::new("a".repeat(43))))
                .unwrap();
            Ok(serde_json::json!({"redirect":"/done", "value":input["value"]}))
        },
    );
    let mut protected =
        Definition::typed::<Exchange>(true, vec![], Default::default(), &[], |_, input, _| {
            Ok(input)
        });
    protected.name = "fixture.protected".into();
    protected.http.as_mut().unwrap().operation = "fixture.protected";
    let registry = Registry::default()
        .with_preconnection_request(definition)
        .with_request(protected);
    let routes: Vec<_> = registry.http_routes().collect();
    let host = snap_transport::host::Blocking::new(
        store,
        (),
        registry,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(|_, _| {
            Err(snap_store::Error::NotFound)
        }))),
        Default::default(),
        "boot".into(),
    );
    let server = Server::new(host).await.unwrap();
    let write = cookies.clone();
    let clear = cookies.clone();
    let operations = routes
        .into_iter()
        .map(|route| {
            HttpOperation::from_route(
                route,
                Arc::new({
                    let write = write.clone();
                    move |bearer| write.encode(bearer, false)
                }),
            )
            .parameters("/exchange", |_, params| Ok(serde_json::json!(params)))
            .form_post("https://issuer.test".into())
            .response({
                let clear = clear.clone();
                move |output| {
                    assert_eq!(output["value"], "proof=&+é");
                    let mut response = redirect(output["redirect"].as_str().unwrap())?;
                    cookie(&mut response, clear.encode(None, true))?;
                    Ok(response)
                }
            })
        })
        .collect();
    let router = server.http(
        WebSocket {
            origin: "https://app.test".into(),
            cookie: cookies.reader(),
        },
        operations,
    );
    for method in ["GET", "POST"] {
        let params = "value=proof%3D%26%2B%C3%A9";
        let request = if method == "GET" {
            Request::builder()
                .uri(format!("/exchange?{params}"))
                .body(Body::empty())
                .unwrap()
        } else {
            Request::builder()
                .method(method)
                .uri("/exchange")
                .header("origin", "https://issuer.test")
                .header(
                    "content-type",
                    "application/x-www-form-urlencoded; charset=utf-8",
                )
                .body(Body::from(params))
                .unwrap()
        };
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], "/done");
        assert_eq!(response.headers()["cache-control"], "no-store");
        let values: Vec<_> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        assert!(
            values
                .iter()
                .any(|v| v.starts_with("fixture_login=;") && v.contains("Max-Age=0"))
        );
        let mut headers = HeaderMap::new();
        for value in &values {
            headers.append("cookie", value.split(';').next().unwrap().parse().unwrap());
        }
        assert_eq!(cookies.read(&headers, false), Some("a".repeat(43)));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    for (method, uri, origin, content_type, body, expected) in [
        ("GET", "/exchange?value=x&value=y", "", "", "", 400),
        ("GET", "/exchange?value=x&%76alue=y", "", "", "", 400),
        ("GET", "/exchange?value=%FF", "", "", "", 400),
        ("GET", "/exchange?value=%zz", "", "", "", 400),
        ("GET", "/exchange?value=x", "", "", "unexpected", 400),
        (
            "POST",
            "/exchange",
            "https://attacker.test",
            "application/x-www-form-urlencoded",
            "value=x",
            403,
        ),
        (
            "POST",
            "/exchange",
            "",
            "application/x-www-form-urlencoded",
            "value=x",
            403,
        ),
        (
            "POST",
            "/exchange?value=x",
            "https://issuer.test",
            "application/x-www-form-urlencoded",
            "value=y",
            400,
        ),
        (
            "POST",
            "/exchange",
            "https://issuer.test",
            "application/json",
            "{}",
            400,
        ),
        (
            "POST",
            "/exchange",
            "https://issuer.test",
            "application/x-www-form-urlencoded",
            "value=x&value=y",
            400,
        ),
        ("GET", "/fixture/protected", "", "", "", 404),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("origin", origin)
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected, "{uri}");
        assert!(response.headers().get("set-cookie").is_none());
        assert!(response.headers().get("location").is_none());
    }
    let oversized = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/exchange?value={}", "a".repeat(32 * 1024)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "malformed ingress must not dispatch"
    );
    let response = router
        .oneshot(
            Request::builder()
                .uri("/exchange?mode=failure")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        response.headers().get("set-cookie").is_none(),
        "initial staged bearer is private"
    );
    assert!(response.headers().get("location").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn signed_cookie_namespace_tampering_and_restart_key_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("http.sqlite");
    snap_store_sqlite::migrate(&database, &[toml::from_str(Cookies::MIGRATION).unwrap()]).unwrap();
    let mut store = snap_store_sqlite::Sqlite::open(&database).unwrap();
    let cookies = Cookies::load(&mut store, "chatty", true).unwrap();
    let bearer = "a".repeat(43);
    let wire = cookies.encode(Some(&bearer), false);
    assert!(wire.starts_with("__Host-chatty_session="));
    assert!(wire.contains("; HttpOnly; SameSite=Lax;"));
    assert!(wire.ends_with("; Secure"));
    let raw = wire.split(';').next().unwrap();
    let read = |value: &str, correlation| {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", value.parse().unwrap());
        cookies.read(&headers, correlation)
    };
    assert_eq!(read(raw, false), Some(bearer.clone()));
    assert_eq!(read(&raw.replace("session", "login"), true), None);
    assert_eq!(
        read(&raw.replacen(&bearer, &"b".repeat(43), 1), false),
        None
    );
    assert_eq!(read(&format!("{raw}; {raw}"), false), None);
    assert_eq!(read(raw, true), None);
    drop(store);
    let mut reopened = snap_store_sqlite::Sqlite::open(&database).unwrap();
    let restarted = Cookies::load(&mut reopened, "chatty", true).unwrap();
    assert_eq!(restarted.encode(Some(&bearer), false), wire);
    let mut headers = HeaderMap::new();
    headers.insert("cookie", raw.parse().unwrap());
    assert_eq!(restarted.read(&headers, false), Some(bearer));
    let other = Cookies::load(&mut reopened, "factorio", true).unwrap();
    headers.insert("cookie", raw.replace("chatty", "factorio").parse().unwrap());
    assert_eq!(other.read(&headers, false), None);
}
