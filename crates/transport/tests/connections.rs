use snap_transport::{Error, Event, Invocation, Outcome, Value, json, server::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

thread_local! { static OBJECT: RefCell<Weak<()>> = RefCell::default(); }
#[derive(Default)]
struct State {
    object: Option<Rc<()>>,
}
struct App {
    operations: [Operation<State>; 1],
}
impl App {
    fn new() -> Self {
        Self {
            operations: [Operation {
                key: "start",
                identity_required: true,
                input: Value::is_null,
                output: Value::is_null,
                error: |_| false,
                guard: |_, _, _| Ok(()),
                handle: |_, state, _| {
                    let object = Rc::new(());
                    OBJECT.with(|weak| *weak.borrow_mut() = Rc::downgrade(&object));
                    state.object = Some(object);
                    Ok(Value::Null)
                },
            }],
        }
    }
}
impl Application for App {
    type State = State;
    fn operations(&self) -> &[Operation<State>] {
        &self.operations
    }
}
fn call(id: u64) -> Invocation {
    Invocation {
        id,
        operation: "start".into(),
        input: Value::Null,
    }
}
fn result(events: &mut Vec<Event>) -> Outcome {
    match events.pop().unwrap() {
        Event::Completed { outcome, .. } => outcome,
        _ => panic!("missing completion"),
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
        App::new(),
        resolver,
        Config {
            reconnect_ms: 100,
            capacity: 2,
        },
    )
    .unwrap();
    let (old, _) = server.connect("old", "tab", 0).unwrap();
    let mut events = Vec::new();
    server.invoke(&old, call(1), |e| events.push(e));
    assert!(result(&mut events).is_ok());
    assert_eq!(
        calls.get(),
        1,
        "connected invocation must not resolve again"
    );
    *token.borrow_mut() = "new";
    server.disconnect(&old, 1).unwrap();
    assert!(matches!(
        server.connect("old", "tab", 2),
        Err(Error::InvalidBearer)
    ));
    let (new, resumed) = server.connect("new", "tab", 2).unwrap();
    assert!(resumed);
    assert_eq!(server.disconnect(&old, 3), Err(Error::StaleConnection));
    assert_eq!(server.close(&old), Err(Error::StaleConnection));
    server.invoke(&old, call(2), |e| events.push(e));
    assert_eq!(result(&mut events), Err(Error::StaleConnection));
    assert!(matches!(
        server.connect("new", "tab", 3),
        Err(Error::Occupied)
    ));
    let (bob, resumed) = server.connect("bob", "tab", 3).unwrap();
    assert!(!resumed);
    assert!(matches!(
        server.connect("bob", "other", 3),
        Err(Error::Capacity)
    ));
    server.close(&bob).unwrap();
    server.close(&new).unwrap();
}

#[test]
fn residency_releases_actual_references_on_close_and_expiry() {
    let mut server = Server::new(
        App::new(),
        |_: &str| Some("id".into()),
        Config {
            reconnect_ms: 10,
            capacity: 1,
        },
    )
    .unwrap();
    let (first, _) = server.connect("token", "tab", 0).unwrap();
    server.invoke(&first, call(1), |_| {});
    let weak = OBJECT.with(|weak| weak.borrow().clone());
    server.disconnect(&first, 1).unwrap();
    server.tick(10);
    assert!(weak.upgrade().is_some());
    let (second, resumed) = server.connect("token", "tab", 10).unwrap();
    assert!(resumed);
    server.close(&second).unwrap();
    assert!(weak.upgrade().is_none());
    let (third, _) = server.connect("token", "tab", 11).unwrap();
    server.invoke(&third, call(1), |_| {});
    let weak = OBJECT.with(|weak| weak.borrow().clone());
    server.disconnect(&third, 12).unwrap();
    server.tick(22);
    assert!(weak.upgrade().is_none());
    assert_eq!(server.resident_count(), 0);
}

#[test]
fn acceptance_precedes_handler_and_duplicate_ids_do_not_execute() {
    let mut server =
        Server::new(App::new(), |_: &str| Some("id".into()), Default::default()).unwrap();
    let (attachment, _) = server.connect("token", "tab", 0).unwrap();
    OBJECT.with(|weak| *weak.borrow_mut() = Weak::new());
    server.invoke(&attachment, call(1), |event| {
        if let Event::Accepted { .. } = event {
            assert!(OBJECT.with(|weak| weak.borrow().upgrade().is_none()));
        }
    });
    let original = OBJECT.with(|weak| weak.borrow().upgrade().unwrap());
    let mut events = Vec::new();
    server.invoke(&attachment, call(1), |e| events.push(e));
    assert_eq!(
        events,
        vec![Event::Completed {
            id: 1,
            outcome: Err(Error::Protocol)
        }]
    );
    assert!(Rc::ptr_eq(
        &original,
        &OBJECT.with(|weak| weak.borrow().upgrade().unwrap())
    ));
}

#[test]
fn result_and_error_schemas_are_enforced() {
    for outcome in [Ok(json!(5)), Err(Error::Application(json!("undeclared")))] {
        let mut app = App::new();
        app.operations[0].handle = if outcome.is_ok() {
            |_, _, _| Ok(json!(5))
        } else {
            |_, _, _| Err(Error::Application(json!("undeclared")))
        };
        let server = Server::new(app, |_: &str| Some("id".into()), Default::default()).unwrap();
        let mut events = Vec::new();
        server.request(Some("token"), call(1), |e| events.push(e));
        assert_eq!(
            events,
            vec![
                Event::Accepted { id: 1 },
                Event::Completed {
                    id: 1,
                    outcome: Err(Error::InvalidOutput)
                }
            ]
        );
    }
}
