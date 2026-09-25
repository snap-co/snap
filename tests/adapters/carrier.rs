//! Independent Protocol provider used by the carrier and execution contracts.
use snap_protocol::{Invocation, Operation, Provider, json};
use snap_web::{Lease, Reply};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
pub struct App {
    gate: Rc<RefCell<VecDeque<futures_channel::oneshot::Sender<()>>>>,
    started: Rc<Cell<bool>>,
    completed: Rc<Cell<usize>>,
    reads_started: Rc<Cell<usize>>,
    now: fn() -> u64,
}
impl App {
    pub fn new(now: fn() -> u64) -> Self {
        Self {
            gate: Rc::default(),
            started: Rc::default(),
            completed: Rc::default(),
            reads_started: Rc::default(),
            now,
        }
    }
}
impl Provider for App {
    type Context = Option<String>;
    type Output = Reply;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        [
            "echo",
            "lease.resolve",
            "hold",
            "read.wait",
            "release",
            "status",
        ]
        .into_iter()
        .map(|key| {
            Operation::new(
                key,
                snap_protocol::any_input,
                snap_protocol::IdentityPolicy::Optional,
            )
        })
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        token: Option<String>,
    ) -> impl core::future::Future<
        Output = Result<snap_protocol::Accepted<Reply>, snap_protocol::Error>,
    > + 'static {
        let gate = self.gate.clone();
        let started = self.started.clone();
        let completed = self.completed.clone();
        let reads_started = self.reads_started.clone();
        let now = self.now;
        core::future::ready(Ok(snap_protocol::Accepted::new(move || async move {
            match invocation.key.as_str() {
                "lease.resolve" => {
                    let mut reply = Reply::new(Ok(json!({})));
                    if token.as_deref() == Some("fixture") {
                        reply.lease = Some(Lease {
                            id: "fixture".into(),
                            expires_at: now() + 60_000,
                        });
                    }
                    reply
                }
                "hold" | "read.wait" => {
                    let read = invocation.key == "read.wait";
                    if read {
                        reads_started.set(reads_started.get() + 1);
                    }
                    started.set(true);
                    let (send, receive) = futures_channel::oneshot::channel();
                    gate.borrow_mut().push_back(send);
                    let _ = receive.await;
                    completed.set(completed.get() + 1);
                    Reply::new(Ok(if read {
                        invocation.payload.unwrap_or_default()
                    } else {
                        json!("completed")
                    }))
                }
                "release" => {
                    if let Some(send) = gate.borrow_mut().pop_front() {
                        let _ = send.send(());
                    }
                    Reply::new(Ok(json!("released")))
                }
                "status" => Reply::new(Ok(
                    json!({"started":started.get(),"completed":completed.get(),"readsStarted":reads_started.get()}),
                )),
                _ => Reply::new(Ok(invocation.payload.unwrap_or_default())),
            }
        })))
    }
}
pub fn bindings() -> Vec<snap_web::Binding> {
    let mut bindings = vec![snap_web::Binding {
        key: "echo",
        http: Some(snap_web::Method::Post),
        socket: true,
    }];
    bindings.push(snap_web::Binding {
        key: "read.wait",
        http: Some(snap_web::Method::Post),
        socket: true,
    });
    bindings.extend(
        ["hold", "release", "status"]
            .into_iter()
            .map(|key| snap_web::Binding {
                key,
                http: Some(snap_web::Method::Post),
                socket: false,
            }),
    );
    bindings
}
