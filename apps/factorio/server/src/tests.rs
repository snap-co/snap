use crate::effects;
use factorio::{Candidate, Config, Phase, Session};
use std::path::Path;
#[path = "../../../../crates/platform/transport-tcp/tests/support/mod.rs"]
mod tls_support;

const ROOT: &str = "a0000000-0000-4000-8000-000000000001";

fn operations(
    document: &std::sync::Arc<snap_document::server::Document>,
    config: factorio::Config,
) -> snap_transport::operation::Registry {
    let mut registry = snap_transport::operation::Registry::default();
    for definition in snap_document::operations::definitions(document.clone()) {
        registry = registry.with_request(definition);
    }
    crate::operations::register(registry, config, "http://localhost".into())
}

#[test]
fn accepted_operations_use_captured_authority_while_new_admissions_reject() {
    use factorio::workspaces as graph;
    use snap_document::runtime::Runtime as Host;
    use snap_transport::{Command, Event, Invocation, Response, json};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for operation in [
        "factorio.identity",
        "factorio.command",
        "factorio.intake-drafts",
    ] {
        let (mut store, config, session) = authority_fixture();
        let owner = session.owner.clone();
        store
            .run("seed intake", |tx| {
                factorio::intake::create(tx, ROOT, &owner, "request", "A bounded change")?;
                Ok(())
            })
            .unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let authority = live.clone();
        let document = Arc::new(factorio::workspaces::document());
        let mut host = Host::new(
            store,
            document.clone(),
            operations(&document, config),
            Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
                move |tx, bearer| {
                    if !authority.load(Ordering::SeqCst) {
                        return Err(snap_store::Error::NotFound);
                    }
                    crate::operations::session(tx, bearer).map(|(s, _)| s.owner)
                },
            ))),
            Default::default(),
            "accepted-authority".into(),
        )
        .with_inputs(crate::operations::inputs);
        let peer = host.open().unwrap();
        host.submit(
            peer,
            Command::Connect {
                bearer: "browser".into(),
                client_id: "identity-test".into(),
            },
            0,
        )
        .unwrap();
        assert_eq!(
            host.drain(peer).unwrap(),
            vec![Response::Attached { resumed: false }]
        );
        let ticket = json!({"id":"request-first","title":"Accepted change","description":"","modules":["one"],"status":"draft","notes":"","parent":null,"blockers":[]});
        let input = match operation {
            "factorio.command" => {
                json!({"workspace":ROOT,"command":{"command":"ticket","ticket":ticket}})
            }
            "factorio.intake-drafts" => {
                json!({"workspace":ROOT,"id":"request","drafts":{"revision":0,"route":"implement","rationale":"Agreed","tickets":[ticket]}})
            }
            _ => json!({}),
        };
        let invoke = |id| {
            Command::Invoke(Invocation {
                id,
                operation: operation.into(),
                input: input.clone(),
            })
        };
        host.submit(peer, invoke(1), 0).unwrap();
        assert_eq!(
            host.drain(peer).unwrap(),
            vec![Response::Event(Event::Accepted { id: 1 })]
        );
        live.store(false, Ordering::SeqCst);
        assert!(host.step());
        let completion = host.drain(peer).unwrap().into_iter().find_map(|response| {
            let Response::Event(event) = response else {
                return None;
            };
            [event].into_iter().find_map(|event| match event {
                Event::Completed { id: 1, outcome } => Some(outcome),
                _ => None,
            })
        });
        let outcome = completion.expect("accepted operation must settle").unwrap();
        host.transact("committed accepted work", |tx| {
            let state = graph::load(tx, ROOT, &owner)?;
            if operation == "factorio.identity" {
                assert_eq!(outcome, json!({"owner":owner}));
            } else {
                assert_eq!(state.tickets["request-first"].title, "Accepted change");
                if operation == "factorio.intake-drafts" {
                    assert_eq!(state.intakes["request"].revision, 1);
                }
            }
            Ok(())
        })
        .unwrap();
        assert!(host.submit(peer, invoke(2), 1).is_err());
        assert!(!host.step());
    }
}

#[test]
fn revoked_and_expired_sessions_cannot_admit_workspace_operations() {
    use factorio::workspaces as graph;
    use snap_document::runtime::Runtime as Host;
    use snap_identity::oauth as rp;
    use snap_transport::{Command, Invocation, Response, json};
    use std::sync::Arc;
    for loss in ["revoked", "session-expired", "access-expired"] {
        let (store, config, mut session) = authority_fixture();
        let owner = session.owner.clone();
        let document = Arc::new(graph::document());
        let mut host = Host::new(
            store,
            document.clone(),
            operations(&document, config),
            Arc::new(snap_transport::bearer::Callbacks::with_retained(
                Arc::new(|tx, bearer| crate::operations::session(tx, bearer).map(|(s, _)| s.owner)),
                Arc::new(crate::operations::retained),
            )),
            Default::default(),
            "expired-authority".into(),
        )
        .with_inputs(crate::operations::inputs);
        let peer = host.open().unwrap();
        host.submit(
            peer,
            Command::Connect {
                bearer: "browser".into(),
                client_id: "authority-test".into(),
            },
            0,
        )
        .unwrap();
        assert_eq!(
            host.drain(peer).unwrap(),
            vec![Response::Attached { resumed: false }]
        );
        let edit = |id, title| {
            Command::Invoke(Invocation {
                id,
                operation: "factorio.command".into(),
                input: json!({"workspace":ROOT,"command":{"command":"ticket","ticket":{"id":"one","title":title,"description":"","modules":["one"],"status":"draft","notes":"","parent":null,"blockers":[]}}}),
            })
        };
        host.submit(peer, edit(1, "Allowed"), 0).unwrap();
        assert!(host.step());
        host.transact("valid command committed", |tx| {
            assert_eq!(
                graph::load(tx, ROOT, &owner)?.tickets["one"].title,
                "Allowed"
            );
            Ok(())
        })
        .unwrap();
        host.drain(peer).unwrap();
        host.transact("lose authority", |tx| {
            if loss == "revoked" {
                rp::revoke(tx, "browser")
            } else {
                if loss == "session-expired" {
                    session.expires = 0;
                } else {
                    session.tokens.access_expires = 0;
                }
                tx.update(
                    "identity.oauth_grants",
                    &[session.id.clone().into()],
                    [(
                        "data".into(),
                        serde_json::to_string(&session).unwrap().into(),
                    )]
                    .into_iter()
                    .collect(),
                )
            }
        })
        .unwrap();
        if host.submit(peer, edit(2, "Denied"), 1).is_ok() {
            // A renewable login may keep its attachment while current access is
            // expired. Admission must reject the operation, not erase recovery.
            let events: Vec<_> = host
                .drain(peer)
                .unwrap()
                .into_iter()
                .flat_map(|r| match r {
                    Response::Event(event) => vec![event],
                    _ => vec![],
                })
                .collect();
            assert!(events.iter().any(|event| matches!(
                event,
                snap_transport::Event::Completed {
                    id: 2,
                    outcome: Err(_)
                }
            )));
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, snap_transport::Event::Accepted { id: 2 }))
            );
        }
        assert!(!host.step());
        host.transact("nothing persisted", |tx| {
            let state = graph::load(tx, ROOT, &owner)?;
            assert_eq!(state.tickets["one"].title, "Allowed");
            assert!(state.intakes.is_empty());
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn detached_cli_recovery_survives_access_expiry_but_not_login_expiry_or_revocation() {
    use snap_document::runtime::Runtime as Host;
    use snap_identity::oauth as rp;
    use snap_transport::{Command, Invocation, Response, json, server::Config};
    use std::sync::Arc;
    for loss in [
        "access-expired",
        "refreshing",
        "local-expired",
        "session-expired",
        "revoked",
    ] {
        let (mut store, config, mut session) = authority_fixture();
        store
            .run("seed CLI credential", |tx| {
                tx.insert(
                    "factorio.cli",
                    [
                        ("id".into(), rp::digest("cli").into()),
                        ("session".into(), session.id.clone().into()),
                        ("expires".into(), session.expires.into()),
                    ]
                    .into_iter()
                    .collect(),
                )
            })
            .unwrap();
        let document = Arc::new(factorio::workspaces::document());
        let mut host = Host::new(
            store,
            document.clone(),
            operations(&document, config),
            Arc::new(snap_transport::bearer::Callbacks::with_retained(
                Arc::new(|tx, bearer| crate::operations::session(tx, bearer).map(|(s, _)| s.owner)),
                Arc::new(crate::operations::retained),
            )),
            Config {
                reconnect_ms: 1_800_000,
                capacity: 8,
            },
            "retention".into(),
        )
        .with_inputs(crate::operations::inputs);
        let connect = || Command::Connect {
            bearer: "cli".into(),
            client_id: "retained".into(),
        };
        let invoke = || {
            Command::Invoke(Invocation {
                id: 1,
                operation: "factorio.identity".into(),
                input: json!({}),
            })
        };
        let peer = host.open().unwrap();
        host.submit(peer, connect(), 0).unwrap();
        let lifetime = host.attachment_lifetime(peer).unwrap();
        host.drain(peer).unwrap();
        host.submit(peer, invoke(), 0).unwrap();
        assert!(host.step());
        let original: Vec<_> = host
            .drain(peer)
            .unwrap()
            .into_iter()
            .filter(|response| matches!(response, Response::Event(_)))
            .collect();
        host.carrier_control(peer).unwrap().detach(1);
        host.tick(1);
        host.transact("lose access or login", |tx| {
            if loss == "revoked" {
                return rp::revoke_id(tx, &session.id);
            }
            if loss == "local-expired" {
                tx.update(
                    "factorio.cli",
                    &[rp::digest("cli").into()],
                    [("expires".into(), 0.into())].into_iter().collect(),
                )?;
            }
            if loss == "session-expired" {
                session.expires = 0;
            }
            session.tokens.access_expires = 0;
            session.refreshing = loss == "refreshing";
            tx.update(
                "identity.oauth_grants",
                &[session.id.clone().into()],
                [(
                    "data".into(),
                    serde_json::to_string(&session).unwrap().into(),
                )]
                .into_iter()
                .collect(),
            )
        })
        .unwrap();
        host.tick(610_000);
        let replacement = host.open().unwrap();
        // Retention is not access authority: even a surviving lifetime cannot
        // admit a new connection/operation before its tokens have been refreshed.
        assert!(host.submit(replacement, connect(), 610_000).is_err());
        if matches!(loss, "access-expired" | "refreshing") {
            host.transact("owned refresh settled", |tx| {
                session.tokens.access_expires = crate::now() + 600;
                session.refreshing = false;
                tx.update(
                    "identity.oauth_grants",
                    &[session.id.clone().into()],
                    [(
                        "data".into(),
                        serde_json::to_string(&session).unwrap().into(),
                    )]
                    .into_iter()
                    .collect(),
                )
            })
            .unwrap();
            host.submit(replacement, connect(), 610_001).unwrap();
            assert_eq!(
                host.drain(replacement).unwrap(),
                vec![Response::Attached { resumed: true }]
            );
            assert_eq!(host.attachment_lifetime(replacement).unwrap(), lifetime);
            host.submit(replacement, invoke(), 610_001).unwrap();
            // The retained completion is returned; the application is not replayed.
            assert_eq!(host.drain(replacement).unwrap(), original);
            assert!(!host.step());
            host.carrier_control(replacement).unwrap().detach(610_002);
            host.tick(610_002);
            host.tick(2_410_002);
            let expired = host.open().unwrap();
            host.submit(expired, connect(), 2_410_002).unwrap();
            assert_eq!(
                host.drain(expired).unwrap(),
                vec![Response::Attached { resumed: false }]
            );
            assert_ne!(host.attachment_lifetime(expired).unwrap(), lifetime);
        } else {
            assert!(host.retired(peer));
        }
    }
}

#[test]
fn retained_login_cannot_receive_new_holdings_until_access_is_valid() {
    use factorio::workspaces as graph;
    use snap_document::runtime::Runtime as Host;
    use snap_transport::{Command, Invocation, Response, json};
    use std::sync::Arc;
    let (store, config, mut session) = authority_fixture();
    let document = Arc::new(graph::document());
    let mut host = Host::new(
        store,
        document.clone(),
        operations(&document, config),
        Arc::new(snap_transport::bearer::Callbacks::with_retained(
            Arc::new(|tx, bearer| crate::operations::session(tx, bearer).map(|(s, _)| s.owner)),
            Arc::new(crate::operations::retained),
        )),
        Default::default(),
        "holdings-authority".into(),
    )
    .with_inputs(crate::operations::inputs);
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: "browser".into(),
            client_id: "watch".into(),
        },
        0,
    )
    .unwrap();
    host.drain(peer).unwrap();
    host.submit(
        peer,
        Command::Invoke(Invocation {
            id: 1,
            operation: "factorio.identity".into(),
            input: json!({}),
        }),
        0,
    )
    .unwrap();
    assert!(host.step());
    assert!(
        host.drain(peer)
            .unwrap()
            .iter()
            .any(|r| matches!(r, Response::Global { .. }))
    );
    session.tokens.access_expires = 0;
    host.transact("expire access and change watched state", |tx| {
        tx.update(
            "identity.oauth_grants",
            &[session.id.clone().into()],
            [(
                "data".into(),
                serde_json::to_string(&session).unwrap().into(),
            )]
            .into_iter()
            .collect(),
        )?;
        factorio::intake::create(
            tx,
            ROOT,
            &session.owner,
            "private-update",
            "Not yet visible",
        )?;
        Ok(())
    })
    .unwrap();
    host.tick(1);
    assert!(!host.retired(peer));
    assert!(
        host.drain(peer).unwrap().is_empty(),
        "retention must not authorize new watched data"
    );
    session.tokens.access_expires = crate::now() + 600;
    host.transact("access renewed", |tx| {
        tx.update(
            "identity.oauth_grants",
            &[session.id.clone().into()],
            [(
                "data".into(),
                serde_json::to_string(&session).unwrap().into(),
            )]
            .into_iter()
            .collect(),
        )
    })
    .unwrap();
    assert!(
        host.drain(peer)
            .unwrap()
            .iter()
            .any(|r| matches!(r, Response::Global { .. }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real TLS maintenance and slow controller suite"]
async fn two_maintained_tcp_connections_do_not_deadlock_controller_io() {
    use snap_document::runtime::Runtime as Host;
    use snap_identity_native::oauth::{Cookies, OAuth};
    use snap_transport::{Command, Event, Invocation, Response, json};
    use snap_transport_native::{Dispatcher, Shared};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    let (mut store, config, _) = authority_fixture();
    let cookies = Cookies::load(&mut store, "factorio", false).unwrap();
    let document = Arc::new(factorio::workspaces::document());
    let host = Host::new(
        store,
        document.clone(),
        operations(&document, config),
        Arc::new(snap_transport::bearer::Callbacks::with_retained(
            Arc::new(|tx, bearer| crate::operations::session(tx, bearer).map(|(s, _)| s.owner)),
            Arc::new(crate::operations::retained),
        )),
        Default::default(),
        "slow-controller".into(),
    )
    .with_inputs(crate::operations::inputs);
    let runtime = tokio::runtime::Handle::current();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let mut entered = Some(entered);
    let host = host.with_controller(
        factorio::workspaces::WORKSPACE_KIND,
        Box::new(move |_, _| {
            if let Some(entered) = entered.take() {
                entered.send(()).unwrap();
                // Production native controllers await Tokio process IO while
                // holding this same host gate. Two renewal waits must leave the
                // two async workers free to drive that IO and this deadline.
                runtime.block_on(tokio::time::sleep(Duration::from_secs(17)));
            }
            Ok(())
        }),
    );
    let shared = Shared::new(host);
    let oauth = OAuth::new(
        shared.clone(),
        cookies,
        snap_identity_native::oauth::Config {
            origin: "http://localhost".into(),
            issuer: "http://localhost:3846".into(),
            client: "factorio".into(),
            secret: String::from("fixture-secret-with-at-least-32-bytes").into(),
            dev_origins: vec![],
        },
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let prepare: snap_transport_native::Prepare = Arc::new({
        let calls = calls.clone();
        move |command| {
            let oauth = oauth.clone();
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                crate::login::prepare(&oauth, command)
                    .await
                    .map_err(|_| snap_transport::Error::InvalidBearer)
            })
        }
    });
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let (server_tls, client_tls) = tls_support::pki(directory.path(), false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let serving = shared.clone();
    let _server = Stop(tokio::spawn(async move {
        snap_transport_tcp::serve(
            listener,
            Dispatcher::tcp(serving, Some(prepare)),
            server_tls,
        )
        .await
        .unwrap();
    }));
    let mut clients = Vec::new();
    for id in ["one", "two"] {
        let mut client = snap_transport_tcp::Client::open(&address, &client_tls)
            .await
            .unwrap();
        client
            .send(&Command::Connect {
                bearer: "browser".into(),
                client_id: id.into(),
            })
            .await
            .unwrap();
        assert_eq!(
            client.receive().await.unwrap().0,
            Response::Attached { resumed: false }
        );
        clients.push(client);
    }
    let _dispatcher = Stop(tokio::spawn(snap_transport_native::dispatch(shared)));
    clients[0].send(&Command::Invoke(Invocation {
        id: 1, operation: "factorio.command".into(), input: json!({"workspace":ROOT,"command":{"command":"ticket","ticket":{"id":"one","title":"Trigger controller","description":"","modules":["one"],"status":"draft","notes":"","parent":null,"blockers":[]}}}),
    })).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        waiting.await.unwrap();
        loop {
            if let Response::Event(event) = clients[0].receive().await.unwrap().0
                && matches!(
                    event,
                    Event::Completed {
                        id: 1,
                        outcome: Ok(_)
                    }
                )
            {
                break;
            }
        }
        assert!(
            calls.load(Ordering::SeqCst) >= 4,
            "both maintained carriers must wait during the controller"
        );
        clients[1]
            .send(&Command::Invoke(Invocation {
                id: 2,
                operation: "factorio.identity".into(),
                input: json!({}),
            }))
            .await
            .unwrap();
        loop {
            if let Response::Event(event) = clients[1].receive().await.unwrap().0
                && matches!(
                    event,
                    Event::Completed {
                        id: 2,
                        outcome: Ok(_)
                    }
                )
            {
                break;
            }
        }
    })
    .await
    .expect("controller IO and deferred renewal must settle on a two-worker runtime");
}

#[tokio::test]
#[ignore = "real OAuth IO suite"]
async fn disconnected_refresh_waiter_does_not_cancel_owned_exchange_or_replay_it() {
    use snap_document::runtime::Runtime as Host;
    use snap_identity::oauth as rp;
    use snap_identity_native::oauth::{Cookies, OAuth};
    use snap_transport_native::Shared;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    let (mut store, _, mut session) = authority_fixture();
    session.tokens.access_expires = 0;
    store
        .run("expire access", |tx| {
            tx.update(
                "identity.oauth_grants",
                &[session.id.clone().into()],
                [(
                    "data".into(),
                    serde_json::to_string(&session).unwrap().into(),
                )]
                .into_iter()
                .collect(),
            )
        })
        .unwrap();
    let cookies = Cookies::load(&mut store, "factorio", false).unwrap();
    let document = Arc::new(factorio::workspaces::document());
    let mut registry = snap_transport::operation::Registry::default();
    for definition in snap_document::operations::definitions(document.clone()) {
        registry = registry.with_request(definition);
    }
    let host = Host::new(
        store,
        document,
        registry,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |tx, bearer| crate::operations::session(tx, bearer).map(|(s, _)| s.owner),
        ))),
        Default::default(),
        "cancelled-refresh".into(),
    );
    let shared = Shared::new(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let router = axum::Router::new().route(
        "/.well-known/openid-configuration",
        axum::routing::get({
            let entered = entered.clone();
            let release = release.clone();
            let calls = calls.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.notified().await;
                    axum::http::StatusCode::BAD_GATEWAY
                }
            }
        }),
    );
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Stop(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let oauth = OAuth::new(
        shared,
        cookies,
        snap_identity_native::oauth::Config {
            origin: "http://localhost".into(),
            issuer,
            client: "factorio".into(),
            secret: String::from("fixture-secret-with-at-least-32-bytes").into(),
            dev_origins: vec![],
        },
    )
    .unwrap();
    let request = oauth.clone();
    let id = session.id.clone();
    let waiter = tokio::spawn(async move { request.session_id(&id).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let id = session.id.clone();
            if oauth
                .run_async("retired after failed exchange", move |tx| {
                    rp::retained(tx, &id, crate::now())
                })
                .await
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned failed refresh must retire its session even with no waiter");
    assert!(oauth.session_id(&session.id).await.is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "uncertain exchange must not be retried"
    );
}

fn authority_fixture() -> (
    snap_store::Store<snap_store_sqlite::Sqlite>,
    Config,
    snap_identity::oauth::Grant,
) {
    use factorio::workspaces as graph;
    use snap_identity::oauth as rp;
    let mut store = snap_store_sqlite::Sqlite::memory(&crate::migrations()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(["factorio.agents", "factorio.cli", "factorio.cli_login"].iter())
    {
        store.load(table).unwrap();
    }
    let session = authority_session("browser");
    let config = Config {
        repository: "/repo".into(),
        mainline: "main".into(),
        modules: [("one".into(), "apps/one".into())].into_iter().collect(),
        resources: "/resources".into(),
        first_port: 15000,
        setup: vec![],
        teardown: vec![],
    };
    store
        .run("seed authenticated workspace", |tx| {
            seed_grant(tx, &session)?;
            graph::onboard(tx, ROOT, &session.owner, config.clone())?;
            Ok(())
        })
        .unwrap();
    (store, config, session)
}
fn seed_grant(
    tx: &mut snap_store::Transaction<'_>,
    grant: &snap_identity::oauth::Grant,
) -> Result<(), snap_store::Error> {
    use snap_identity::oauth as rp;
    let state = "fixture-oauth-state-at-least-32-bytes";
    let attempt = rp::Attempt {
        target: None,
        binding: rp::digest("fixture-browser"),
        nonce: grant.nonce.clone(),
        verifier: "verifier".into(),
        redirect: "http://localhost/auth/callback".into(),
        issuer: grant.issuer.clone(),
        old_session: None,
        logout: false,
        expires: crate::now() + 300,
        processing: false,
    };
    rp::start(tx, state, &attempt)?;
    rp::consume(tx, state, "fixture-browser", false, crate::now())?;
    rp::issue(tx, state, grant, crate::now()).map(|_| ())
}

fn authority_session(bearer: &str) -> snap_identity::oauth::Grant {
    use snap_identity::oauth as rp;
    rp::Grant {
        id: rp::digest(bearer),
        owner: rp::owner("http://issuer", "subject"),
        subject: "subject".into(),
        issuer: "http://issuer".into(),
        csrf: "csrf".into(),
        nonce: "nonce".into(),
        profile: serde_json::json!({}),
        tokens: rp::Tokens {
            access: "access".into(),
            refresh: "refresh".into(),
            id_token: "id".into(),
            access_expires: crate::now() + 3600,
            auth_time: None,
        },
        expires: crate::now() + 30 * 24 * 60 * 60,
        refreshing: false,
        version: 1,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real native CLI processes"]
async fn native_cli_login_intake_tools_and_authority_without_shell_environment() {
    use serde_json::json;
    use snap_document::runtime::Runtime as Host;
    use snap_identity::oauth as rp;
    use snap_transport_native::{Dispatcher, Shared};
    let (temp, config, _) = fixture().await;
    let mut store = snap_store_sqlite::Sqlite::memory(&crate::migrations()).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(["factorio.agents", "factorio.cli", "factorio.cli_login"].iter())
    {
        store.load(table).unwrap();
    }
    let session = authority_session("browser");
    let owner = session.owner.clone();
    store
        .run("seed", |tx| {
            seed_grant(tx, &session)?;
            tx.insert(
                "factorio.agents",
                [
                    ("id".into(), rp::digest("agent").into()),
                    ("session".into(), session.id.clone().into()),
                ]
                .into_iter()
                .collect(),
            )?;
            Ok(())
        })
        .unwrap();
    let document = std::sync::Arc::new(factorio::workspaces::document());
    let host = Host::new(
        store,
        document.clone(),
        operations(&document, config),
        std::sync::Arc::new(snap_transport::bearer::Callbacks::new(std::sync::Arc::new(
            |tx, b| crate::operations::session(tx, b).map(|(s, _)| s.owner),
        ))),
        Default::default(),
        "cli-test".into(),
    )
    .with_inputs(crate::operations::inputs);
    let shared = Shared::new(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (server_tls, client_tls) = tls_support::pki(temp.path(), false);
    let tcp = tokio::spawn(snap_transport_tcp::serve(
        listener,
        Dispatcher::tcp(shared.clone(), None),
        server_tls.clone(),
    ));
    let dispatch = tokio::spawn(snap_transport_native::dispatch(shared.clone()));
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("factorio");
    assert!(binary.is_file(), "Build factorio-server before this gate");
    let credentials = temp.path().join("credentials.json");
    async fn call(
        binary: &Path,
        credentials: &Path,
        args: &[&str],
        input: Option<serde_json::Value>,
    ) -> (bool, serde_json::Value, String) {
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(binary)
            .env_clear()
            .arg("--credentials")
            .arg(credentials)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .await
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        (
            result.status.success(),
            serde_json::from_slice(&result.stdout).unwrap_or(serde_json::Value::Null),
            String::from_utf8_lossy(&result.stderr).into(),
        )
    }
    let (ok, login, error) = call(
        &binary,
        &credentials,
        &[
            "login",
            "--addr",
            &addr,
            "--ca-file",
            temp.path().join("ca.pem").to_str().unwrap(),
            "--token",
            "agent",
        ],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(login["owner"], owner);
    assert!(login.get("bearer").is_none());
    assert_eq!(login["expires"].as_i64(), Some(session.expires));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&credentials)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    let (ok, _, error) = call(&binary, &credentials, &["onboard"], None).await;
    assert!(ok, "{error}");
    let (ok, item, error) = call(
        &binary,
        &credentials,
        &["intake", "--no-open", "--", "a local task"],
        None,
    )
    .await;
    assert!(ok, "{error}");
    let id = item["id"].as_str().unwrap();
    let (ok, read, error) = call(
        &binary,
        &credentials,
        &["intake-read", "--intake", id],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(read["intake"]["revision"], 0);
    let drafts = json!({"revision":0,"route":"grill","rationale":"needs scope","tickets":[]});
    let mut watcher = tokio::process::Command::new(&binary)
        .env_clear()
        .arg("--credentials")
        .arg(&credentials)
        .arg("watch")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    use tokio::io::AsyncBufReadExt;
    let mut observations = tokio::io::BufReader::new(watcher.stdout.take().unwrap()).lines();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), observations.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(initial.contains("a local task"));
    let (ok, saved, error) = call(
        &binary,
        &credentials,
        &["intake-save", "-", "--intake", id],
        Some(drafts.clone()),
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(saved["revision"], 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let update = observations.next_line().await.unwrap().unwrap();
            if update.contains("needs scope") {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (ok, _, _) = call(
        &binary,
        &credentials,
        &["intake-save", "-", "--intake", id],
        Some(drafts),
    )
    .await;
    assert!(!ok, "Stale revision must reject");
    // Lose only the completion via a real TCP proxy, after delivering ACK. The
    // Host still owns the accepted draft write; a new process must recover its
    // exact invocation rather than allocating a fresh ID and applying it twice.
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap().to_string();
    let upstream_addr = addr.clone();
    let drop_handshake = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let proxy_drop_handshake = drop_handshake.clone();
    let proxy_server_tls = server_tls.clone();
    let proxy_client_tls = client_tls.clone();
    let proxy = tokio::spawn(async move {
        let mut first = true;
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = proxy_listener.accept() => {
                    let (down,_) = accepted.unwrap();
                    let proxy_server_tls = proxy_server_tls.clone();
                    let proxy_client_tls = proxy_client_tls.clone();
                    let upstream_addr = upstream_addr.clone();
                    let lose = first; first = false;
                    let lose_handshake=proxy_drop_handshake.swap(false,std::sync::atomic::Ordering::SeqCst);
                    peers.spawn(async move {
                        let mut down = proxy_server_tls.accept(down).await.unwrap();
                        let mut up = proxy_client_tls.connect(&upstream_addr).await.unwrap();
                        if !lose && !lose_handshake {let _=tokio::io::copy_bidirectional(&mut down,&mut up).await;return;}
                        let (mut down_read,mut down_write)=tokio::io::split(down);
                        let (mut up_read,mut up_write)=tokio::io::split(up);
                        let forwarding=tokio::spawn(async move {let _=tokio::io::copy(&mut down_read,&mut up_write).await;});
                        loop {
                            let (response,retention)=snap_transport_tcp::read_response(&mut up_read).await.unwrap();
                            if lose_handshake && matches!(response,snap_transport::Response::Attached {..}) {
                                 forwarding.abort();let _=forwarding.await;let _=tokio::io::AsyncWriteExt::shutdown(&mut down_write).await;break;
                            }
                            let accepted=matches!(&response,snap_transport::Response::Event(snap_transport::Event::Accepted {..}));
                            snap_transport_tcp::write_response(&mut down_write,&response,matches!(response,snap_transport::Response::Attached {..}),retention.as_ref()).await.unwrap();
                             if accepted {forwarding.abort();let _=forwarding.await;let _=tokio::io::AsyncWriteExt::shutdown(&mut down_write).await;break;}
                        }
                    });
                },
                Some(_) = peers.join_next(), if !peers.is_empty() => {},
            }
        }
    });
    let (ok,_,_) = call(&binary,&credentials,&["--addr",&proxy_addr,"intake-save","-","--intake",id],Some(json!({"revision":1,"route":"triage","rationale":"accepted but response lost","tickets":[]}))).await;
    assert!(!ok, "The interrupted client must report an unknown outcome");
    // A rejected login must not replace the original recovery endpoint, trust,
    // or identity. Prove it at the saved-state and subsequent retry boundaries.
    let before_login: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert!(before_login["pending"].is_object());
    let alternate_ca = temp.path().join("alternate-ca.pem");
    std::fs::copy(temp.path().join("ca.pem"), &alternate_ca).unwrap();
    let (ok, _, error) = call(
        &binary,
        &credentials,
        &[
            "login",
            "--token",
            "agent",
            "--addr",
            &addr,
            "--ca-file",
            alternate_ca.to_str().unwrap(),
            "--server-name",
            "wrong.example",
            "--workspace",
            "must-not-save",
        ],
        None,
    )
    .await;
    assert!(
        !ok && error.contains("certificate not valid for name"),
        "{error}"
    );
    let after_login: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert_eq!(
        after_login, before_login,
        "Failed login must preserve pending recovery state"
    );
    let (ok, recovered, error) = call(&binary, &credentials, &["retry"], None).await;
    assert!(ok, "{error}");
    assert_eq!(recovered["revision"], 2);
    let (ok, read, error) = call(
        &binary,
        &credentials,
        &["intake-read", "--intake", id],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(read["intake"]["revision"], 2);
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert!(config["next_id"].as_u64().unwrap() > 5);
    let client_id = config["client_id"].as_str().unwrap();
    let mut reattach = snap_transport_tcp::Client::open(&addr, &client_tls)
        .await
        .unwrap();
    reattach
        .send(&snap_transport::Command::Connect {
            bearer: config["bearer"].as_str().unwrap().into(),
            client_id: client_id.into(),
        })
        .await
        .unwrap();
    assert_eq!(
        reattach.receive().await.unwrap().0,
        snap_transport::Response::Attached { resumed: true }
    );
    reattach
        .send(&snap_transport::Command::Close)
        .await
        .unwrap();
    let _ = reattach.receive().await;
    drop(reattach);
    let mut fenced = config;
    fenced["pending"] = json!({"id":fenced["next_id"],"operation":"factorio.intake-drafts","input":{"workspace":fenced["workspace"],"id":id,"drafts":{"revision":2,"route":"triage","rationale":"must never replay on new lifetime","tickets":[]}}});
    fenced["next_id"] = json!(fenced["next_id"].as_u64().unwrap() + 1);
    std::fs::write(&credentials, serde_json::to_vec(&fenced).unwrap()).unwrap();
    drop_handshake.store(true, std::sync::atomic::Ordering::SeqCst);
    let (ok, _, error) = call(&binary, &credentials, &["retry"], None).await;
    assert!(!ok && error.contains("TCP detached"), "{error}");
    let after_loss: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert_eq!(after_loss["pending_replayable"], true);
    assert_eq!(after_loss["lifetime"], fenced["lifetime"]);
    for _ in 0..2 {
        let (ok, _, error) = call(&binary, &credentials, &["retry"], None).await;
        assert!(!ok && error.contains("Logical lifetime ended"), "{error}");
    }
    // Explicit login is the only way to abandon the unknown result here.
    let (ok, _, error) = call(&binary, &credentials, &["login", "--token", "agent"], None).await;
    assert!(ok, "{error}");
    let (ok, read, error) = call(
        &binary,
        &credentials,
        &["intake-read", "--intake", id],
        None,
    )
    .await;
    assert!(ok, "{error}");
    assert_eq!(read["intake"]["revision"], 2);
    for n in 1..=3 {
        let (ok,state,error)=call(&binary,&credentials,&["ticket","-"],Some(json!({"id":format!("large-ticket-{n}"),"title":format!("Large ticket {n}"),"description":"x".repeat(24000),"modules":["a"],"status":"draft","notes":"","parent":null,"blockers":[]}))).await;
        assert!(ok, "{error}");
        assert_eq!(
            state["tickets"][format!("large-ticket-{n}")]["description"]
                .as_str()
                .unwrap()
                .len(),
            24000
        );
    }
    let (ok, state, error) = call(&binary, &credentials, &["status"], None).await;
    assert!(ok, "{error}");
    assert_eq!(state["tickets"].as_object().unwrap().len(), 3);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let update = observations.next_line().await.unwrap().unwrap();
            if update.contains("Large ticket 3") {
                assert!(update.len() > 65536);
                assert!(update.contains("Large ticket 1") && update.contains("Large ticket 2"));
                break;
            }
        }
    })
    .await
    .unwrap();
    watcher.kill().await.unwrap();
    let _ = watcher.wait().await;
    let (ok, _, error) = call(&binary, &credentials, &["logout"], None).await;
    assert!(ok, "{error}");
    let (ok, _, _) = call(&binary, &credentials, &["status"], None).await;
    assert!(!ok);
    tcp.abort();
    dispatch.abort();
    proxy.abort();
    let _ = proxy.await;
    let _ = tcp.await;
    let _ = dispatch.await;
}

#[tokio::test]
#[ignore = "hook crash helper"]
async fn hook_child() {
    let Ok(path) = std::env::var("FACTORIO_HOOK_CHILD") else {
        return;
    };
    let (config, session): (Config, Session) =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    effects::hook(
        &config,
        &session,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "echo $$ > \"$FACTORIO_DATA/running\"; exec sleep 120".into(),
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "real process crash recovery"]
async fn restart_retires_owned_hook_group_after_host_kill() {
    let (_temp, c, s) = fixture().await;
    std::fs::create_dir_all(&s.data).unwrap();
    let input = Path::new(&s.data).join("fixture.json");
    std::fs::write(&input, serde_json::to_vec(&(&c, &s)).unwrap()).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::hook_child", "--ignored"])
        .env("FACTORIO_HOOK_CHILD", input)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let running = Path::new(&s.data).join("running");
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !running.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    child.kill().unwrap();
    child.wait().unwrap();
    effects::reap_hook(&s).unwrap();
    ready.unwrap();
    let pid = std::fs::read_to_string(running).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
            if stat.rsplit_once(") ").unwrap().1.starts_with('Z') {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!Path::new(&s.data).join("process.json").exists());
}
async fn write_commit(dir: &str, path: &str, value: &str) -> String {
    std::fs::write(Path::new(dir).join(path), value).unwrap();
    effects::git(dir, &["add", "--", path]).await.unwrap();
    effects::git(
        dir,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "fixture",
        ],
    )
    .await
    .unwrap();
    effects::git(dir, &["rev-parse", "HEAD"]).await.unwrap()
}
async fn fixture() -> (tempfile::TempDir, Config, Session) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let repo = repo.to_str().unwrap();
    effects::git(repo, &["init", "-b", "main"]).await.unwrap();
    std::fs::create_dir_all(Path::new(repo).join("crates/a")).unwrap();
    let base = write_commit(repo, "crates/a/file", "base").await;
    let c = Config {
        repository: repo.into(),
        mainline: "main".into(),
        modules: [("a".into(), "crates/a".into())].into_iter().collect(),
        resources: temp.path().join("resources").to_str().unwrap().into(),
        first_port: 12000,
        setup: vec![],
        teardown: vec![],
    };
    let worktree = temp.path().join("work").to_str().unwrap().to_owned();
    effects::git(repo, &["worktree", "add", "-b", "factorio/work", &worktree])
        .await
        .unwrap();
    let s = Session {
        id: "work".into(),
        created_at: None,
        owner: "fixture".into(),
        prompt: "fixture".into(),
        tickets: vec![],
        modules: vec!["a".into()],
        phase: Phase::Active,
        base,
        branch: "factorio/work".into(),
        worktree,
        data: temp.path().join("data").to_str().unwrap().into(),
        port: 12000,
        conversation: "ses_fixture".into(),
        candidate: None,
        publications: vec![],
        integration: None,
        error: String::new(),
        desired: factorio::Desired::Active,
    };
    (temp, c, s)
}
#[tokio::test]
#[ignore = "real Git acceptance"]
async fn merge_reconciliation_does_not_merge_twice_and_preserves_dirty_work() {
    let (_temp, c, mut s) = fixture().await;
    let oid = write_commit(&s.worktree, "crates/a/file", "implemented").await;
    let (commit, target) = effects::candidate(&c, &s).await.unwrap();
    assert_eq!(commit, oid);
    s.candidate = Some(Candidate {
        commit,
        target,
        evidence: "fixture".into(),
        findings: vec![],
        approval: None,
    });
    let plan = effects::prepare(&c, &s).await.unwrap();
    s.integration = Some(plan.clone());
    s.phase = Phase::Integrating;
    effects::integrate(&c, &s).await.unwrap();
    // Simulate loss of the Store publication after Git succeeded.
    effects::integrate(&c, &s).await.unwrap();
    assert_eq!(effects::head(&c).await.unwrap(), plan);
    assert_eq!(
        std::fs::read_to_string(Path::new(&c.repository).join("crates/a/file")).unwrap(),
        "implemented"
    );
    s.phase = Phase::Cleanup;
    std::fs::write(Path::new(&s.worktree).join("dirty"), "keep me").unwrap();
    assert!(effects::cleanup(&c, &s).await.is_err());
    assert!(Path::new(&s.worktree).join("dirty").exists());
    std::fs::remove_file(Path::new(&s.worktree).join("dirty")).unwrap();
    effects::cleanup(&c, &s).await.unwrap();
    assert!(!Path::new(&s.worktree).exists());
}
#[tokio::test]
#[ignore = "real Git acceptance"]
async fn moved_candidates_targets_and_unclaimed_changes_stop_integration() {
    let (_temp, c, mut s) = fixture().await;
    write_commit(&s.worktree, "crates/a/file", "first").await;
    let (commit, target) = effects::candidate(&c, &s).await.unwrap();
    s.candidate = Some(Candidate {
        commit,
        target,
        evidence: "fixture".into(),
        findings: vec![],
        approval: None,
    });
    write_commit(&s.worktree, "crates/a/file", "second").await;
    assert!(effects::prepare(&c, &s).await.is_err());
    let (commit, target) = effects::candidate(&c, &s).await.unwrap();
    s.candidate.as_mut().unwrap().commit = commit;
    s.candidate.as_mut().unwrap().target = target;
    write_commit(&c.repository, "mainline-file", "new target").await;
    assert!(effects::prepare(&c, &s).await.is_err());
    write_commit(&s.worktree, "unclaimed", "outside scope").await;
    assert!(
        effects::candidate(&c, &s)
            .await
            .unwrap_err()
            .contains("Unclaimed")
    );
    s.phase = Phase::Abandoning;
    effects::cleanup(&c, &s).await.unwrap();
    assert!(Path::new(&s.worktree).join("unclaimed").exists());
}
