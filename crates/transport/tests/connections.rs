use snap_transport::{Error, Invocation, Value, server::*};
use std::cell::{Cell, RefCell};

fn call(id: u64) -> Invocation {
    Invocation {
        id,
        operation: "operation".into(),
        input: Value::Null,
    }
}

#[test]
fn failed_live_validation_retires_once_and_requires_fresh_attachment() {
    struct Live<'a>(&'a Cell<u8>);
    impl Authority for Live<'_> {
        fn identify(&self, _: &str) -> Result<String, Error> {
            match self.0.get() {
                0 => Ok("alice".into()),
                1 => Err(Error::Unavailable),
                2 => Err(Error::InvalidBearer),
                _ => Ok("bob".into()),
            }
        }
    }
    for fault in 1..=3 {
        let state = Cell::new(0);
        let mut server = Server::new(Live(&state), Config::default()).with_live_authority();
        let (old, _) = server.connect("token", "tab", 0).unwrap();
        state.set(fault);
        let expected = if fault == 1 {
            Error::Unavailable
        } else {
            Error::InvalidBearer
        };
        assert!(matches!(server.invoke(&old, call(1)), Err(error) if error == expected));
        assert!(!server.attached(&old));
        assert_eq!(server.take_retired().len(), 1);
        state.set(0);
        assert!(matches!(
            server.invoke(&old, call(2)),
            Err(Error::StaleConnection)
        ));
        assert!(server.take_retired().is_empty());
        let (fresh, resumed) = server.connect("token", "tab", 1).unwrap();
        assert!(!resumed);
        assert!(matches!(
            server.invoke(&old, call(3)),
            Err(Error::StaleConnection)
        ));
        assert!(server.invoke(&fresh, call(1)).is_ok());
    }
}

#[test]
fn rotation_identity_isolation_and_stale_attachment_fencing() {
    let calls = Cell::new(0);
    let token = RefCell::new("old");
    let resolver = |bearer: &str| {
        calls.set(calls.get() + 1);
        if bearer == *token.borrow() {
            Some("alice".into())
        } else if bearer == "bob" {
            Some("bob".into())
        } else {
            None
        }
    };
    let mut server = Server::new(
        resolver,
        Config {
            reconnect_ms: 100,
            capacity: 2,
        },
    );
    let (old, _) = server.connect("old", "tab", 0).unwrap();
    assert_eq!(
        server.invoke(&old, call(1)).unwrap().identity.as_deref(),
        Some("alice")
    );
    assert_eq!(
        calls.get(),
        1,
        "connected invocation must not resolve again"
    );
    assert!(matches!(server.invoke(&old, call(1)), Err(Error::Protocol)));
    *token.borrow_mut() = "new";
    server.disconnect(&old, 1).unwrap();
    assert!(matches!(
        server.connect("old", "tab", 2),
        Err(Error::InvalidBearer)
    ));
    let (new, resumed) = server.connect("new", "tab", 2).unwrap();
    assert!(resumed);
    assert_eq!(old.connection(), new.connection());
    assert_eq!(server.disconnect(&old, 3), Err(Error::StaleConnection));
    assert_eq!(server.close(&old), Err(Error::StaleConnection));
    assert!(matches!(
        server.invoke(&old, call(2)),
        Err(Error::StaleConnection)
    ));
    assert!(matches!(
        server.connect("new", "tab", 3),
        Err(Error::Occupied)
    ));
    let (bob, resumed) = server.connect("bob", "tab", 3).unwrap();
    assert!(!resumed);
    assert_ne!(bob.connection(), new.connection());
    assert!(matches!(
        server.connect("bob", "other", 3),
        Err(Error::Capacity)
    ));
    server.close(&bob).unwrap();
    server.close(&new).unwrap();
    assert_eq!(
        server.take_retired(),
        vec![bob.connection(), new.connection()]
    );
}

#[test]
fn close_and_expiry_notify_host_exactly_once() {
    let mut server = Server::new(
        |_: &str| Some("id".into()),
        Config {
            reconnect_ms: 10,
            capacity: 1,
        },
    );
    let (first, _) = server.connect("token", "tab", 0).unwrap();
    server.disconnect(&first, 1).unwrap();
    server.tick(10);
    assert!(server.take_retired().is_empty());
    let (second, resumed) = server.connect("token", "tab", 10).unwrap();
    assert!(resumed);
    server.close(&second).unwrap();
    assert_eq!(server.take_retired(), vec![first.connection()]);
    let (third, _) = server.connect("token", "tab", 11).unwrap();
    assert_ne!(first.connection(), third.connection());
    server.disconnect(&third, 12).unwrap();
    server.tick(22);
    assert_eq!(server.take_retired(), vec![third.connection()]);
    server.tick(23);
    assert!(server.take_retired().is_empty());
    assert_eq!(server.resident_count(), 0);
}

#[test]
fn requests_resolve_each_bearer_without_a_resident_scope() {
    let calls = Cell::new(0);
    let server = Server::new(
        |token: &str| {
            calls.set(calls.get() + 1);
            (token == "valid").then(|| "id".into())
        },
        Default::default(),
    );
    assert!(server.request(None, call(1)).unwrap().identity.is_none());
    for id in 2..4 {
        let dispatch = server.request(Some("valid"), call(id)).unwrap();
        assert_eq!(dispatch.identity.as_deref(), Some("id"));
        assert_eq!(dispatch.connection, None);
    }
    assert_eq!(calls.get(), 2);
    assert!(matches!(
        server.request(Some("invalid"), call(4)),
        Err(Error::InvalidBearer)
    ));
    assert_eq!(server.resident_count(), 0);
}

#[test]
fn zero_retention_does_not_let_an_old_handle_detach_a_fresh_connection() {
    // Reduced by Hegel while testing a deliberately removed generation fence.
    let mut server = Server::new(
        |_: &str| Some("alice".into()),
        Config {
            reconnect_ms: 0,
            capacity: 1,
        },
    );
    let (old, _) = server.connect("bearer", "tab", 0).unwrap();
    server.disconnect(&old, 0).unwrap();
    let (fresh, resumed) = server.connect("bearer", "tab", 0).unwrap();
    assert!(!resumed);
    assert_ne!(old.connection(), fresh.connection());
    assert_eq!(server.disconnect(&old, 0), Err(Error::StaleConnection));
    assert!(server.invoke(&fresh, call(1)).is_ok());
    assert_eq!(server.take_retired(), vec![old.connection()]);
}
