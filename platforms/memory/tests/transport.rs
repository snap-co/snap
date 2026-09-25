use futures::{StreamExt, channel::oneshot};
use snap_memory::{Event, Phase, Rig};
use snap_protocol::{
    Accepted, Error, IdentityPolicy, Invocation, Operation, Outcome, Provider, json,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

struct App {
    entered: Rc<Cell<usize>>,
    release: Rc<RefCell<Option<oneshot::Receiver<()>>>>,
}
impl Provider for App {
    type Context = bool;
    type Output = Outcome;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        [Operation::new(
            "work",
            |payload| {
                if payload.as_ref().is_some_and(|v| v.is_boolean()) {
                    Ok(())
                } else {
                    Err(Error::InvalidInputError {
                        message: "boolean required".into(),
                    })
                }
            },
            IdentityPolicy::Required,
        )]
        .into_iter()
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        identified: bool,
    ) -> impl std::future::Future<Output = Result<Accepted<Outcome>, Error>> + 'static {
        let entered = self.entered.clone();
        let release = self.release.clone();
        async move {
            IdentityPolicy::Required.check(identified)?;
            Ok(Accepted::new(move || {
                // Deliberately synchronous handler entry, before its future is polled.
                entered.set(entered.get() + 1);
                let held = release.borrow_mut().take();
                async move {
                    if let Some(held) = held {
                        let _ = held.await;
                    }
                    if invocation.payload == Some(json!(false)) {
                        return Err(Error::OperationError {
                            failure: json!("domain failure"),
                        });
                    }
                    Ok(json!("done"))
                }
            }))
        }
    }
}
fn invocation(value: snap_protocol::Value) -> Invocation {
    Invocation {
        operation_id: "one".into(),
        key: "work".into(),
        payload: Some(value),
        traceparent: None,
    }
}
fn app() -> App {
    App {
        entered: Rc::default(),
        release: Rc::default(),
    }
}

#[test]
fn schema_and_guards_reject_without_handler_or_acceptance() {
    let provider = app();
    let entered = provider.entered.clone();
    let mut rig = Rig::new(provider);
    for (payload, identified) in [(json!("bad"), true), (json!(true), false)] {
        let mut call = rig.submit(invocation(payload), identified);
        assert!(matches!(
            rig.run(call.next()),
            Some(Event::Completed(Err(_)))
        ));
    }
    let mut unknown = invocation(json!(true));
    unknown.key = "missing".into();
    let mut call = rig.submit(unknown, true);
    assert!(matches!(
        rig.run(call.next()),
        Some(Event::Completed(Err(Error::ContractViolationError { .. })))
    ));
    assert_eq!(entered.get(), 0);
    assert!(
        rig.trace()
            .iter()
            .all(|event| event.phase == Phase::Rejected)
    );
}

#[test]
fn acknowledgement_precedes_even_synchronous_handler_entry() {
    let mut provider = app();
    let entered = provider.entered.clone();
    let accepted = futures::executor::block_on(snap_protocol::dispatch(
        &mut provider,
        invocation(json!(true)),
        true,
    ))
    .unwrap();
    assert_eq!(entered.get(), 0);
    let acknowledged = Cell::new(false);
    let future = accepted.start(|| {
        assert_eq!(entered.get(), 0);
        acknowledged.set(true);
    });
    assert!(acknowledged.get());
    assert_eq!(entered.get(), 1);
    assert_eq!(futures::executor::block_on(future).unwrap(), json!("done"));
}

#[test]
fn accepted_work_is_observable_while_pending_and_survives_observer_loss() {
    let provider = app();
    let (send, receive) = oneshot::channel();
    *provider.release.borrow_mut() = Some(receive);
    let mut rig = Rig::with_capacity(provider, 1);
    let mut call = rig.submit(invocation(json!(true)), true);
    assert!(matches!(rig.run(call.next()), Some(Event::Accepted)));
    rig.run_until_stalled();
    assert_eq!(rig.active(), 1);
    let mut refused = rig.submit(invocation(json!(true)), true);
    assert!(matches!(
        rig.run(refused.next()),
        Some(Event::Completed(Err(Error::UnavailableError { .. })))
    ));
    drop(call);
    send.send(()).unwrap();
    rig.run_until_stalled();
    assert_eq!(rig.active(), 0);
    assert_eq!(rig.trace().last().unwrap().phase, Phase::Completed);
    let mut failure = rig.submit(invocation(json!(false)), true);
    assert!(matches!(rig.run(failure.next()), Some(Event::Accepted)));
    assert!(matches!(
        rig.run(failure.next()),
        Some(Event::Completed(Err(Error::OperationError { .. })))
    ));
}

struct GuardContext {
    reservations: Rc<Cell<usize>>,
    deny: bool,
}
struct Reservation(Rc<Cell<usize>>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
struct Guarded(App);
impl Provider for Guarded {
    type Context = GuardContext;
    type Output = Outcome;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        self.0.operations()
    }
    fn guards(&self, _: &str) -> impl Iterator<Item = snap_protocol::Guard<GuardContext>> {
        [reserve as snap_protocol::Guard<GuardContext>, quota].into_iter()
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        _: GuardContext,
    ) -> impl std::future::Future<Output = Result<Accepted<Outcome>, Error>> + 'static {
        self.0.prepare(invocation, true)
    }
}
fn reserve(
    _: &Invocation,
    context: &GuardContext,
) -> snap_protocol::LocalFuture<Result<snap_protocol::GuardLease, Error>> {
    let reservations = context.reservations.clone();
    Box::pin(async move {
        reservations.set(reservations.get() + 1);
        Ok(snap_protocol::GuardLease::new(Reservation(reservations)))
    })
}
fn quota(
    _: &Invocation,
    context: &GuardContext,
) -> snap_protocol::LocalFuture<Result<snap_protocol::GuardLease, Error>> {
    let deny = context.deny;
    Box::pin(async move {
        if deny {
            Err(Error::UnavailableError {
                message: "Quota exhausted".into(),
            })
        } else {
            Ok(snap_protocol::GuardLease::new(()))
        }
    })
}

#[test]
fn operation_guards_release_reservations_on_rejection_and_retain_them_during_work() {
    let provider = app();
    let entered = provider.entered.clone();
    let (send, receive) = oneshot::channel();
    *provider.release.borrow_mut() = Some(receive);
    let reservations = Rc::new(Cell::new(0));
    let mut rig = Rig::new(Guarded(provider));
    let mut rejected = rig.submit(
        invocation(json!(true)),
        GuardContext {
            reservations: reservations.clone(),
            deny: true,
        },
    );
    assert!(matches!(
        rig.run(rejected.next()),
        Some(Event::Completed(Err(_)))
    ));
    assert_eq!(entered.get(), 0);
    assert_eq!(reservations.get(), 0);
    let mut accepted = rig.submit(
        invocation(json!(true)),
        GuardContext {
            reservations: reservations.clone(),
            deny: false,
        },
    );
    assert!(matches!(rig.run(accepted.next()), Some(Event::Accepted)));
    rig.run_until_stalled();
    assert_eq!(reservations.get(), 1);
    send.send(()).unwrap();
    assert!(matches!(
        rig.run(accepted.next()),
        Some(Event::Completed(Ok(_)))
    ));
    assert_eq!(reservations.get(), 0);
}
