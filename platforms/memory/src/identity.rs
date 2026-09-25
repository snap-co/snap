//! Memory execution of the real Identity SDK controller and Passport response
//! projection. Tokens stay in the adapter; callers cannot supply identity claims.
use crate::{Clock, Endpoint, Event, Rig};
use futures::{
    StreamExt,
    channel::oneshot,
    future::{AbortHandle, Abortable},
    task::LocalSpawnExt,
};
use snap_client::identity::{Action, Client as Core, Input, Snapshot};
use snap_protocol::{ConnectionEvent, Disconnect, Error, Invocation, Outcome, Provider, Value};
use snap_runtime::passport::{Context, Response};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

pub(crate) type Bus = Rc<RefCell<Vec<Box<dyn Fn(&[String])>>>>;
pub struct Client<P: Provider<Context = Context, Output = Response>> {
    state: Rc<State<P>>,
}
struct State<P: Provider<Context = Context, Output = Response>> {
    core: RefCell<Core>,
    endpoint: Endpoint<P>,
    clock: Clock,
    token: RefCell<Option<String>>,
    sequence: Cell<u64>,
    pending: RefCell<BTreeMap<u64, oneshot::Sender<Outcome>>>,
    attachment: RefCell<Option<(String, u64)>>,
    jobs: RefCell<Vec<(bool, AbortHandle)>>,
}
impl<P: Provider<Context = Context, Output = Response> + 'static> Rig<P> {
    pub fn client(&self, core: Core) -> Client<P> {
        let bus = self.identity_bus.clone();
        *self.endpoint.project.borrow_mut() = Some(Rc::new(move |reply| {
            for listener in bus.borrow().iter() {
                listener(&reply.revoked);
            }
        }));
        let state = Rc::new(State {
            core: RefCell::new(core),
            endpoint: self.endpoint.clone(),
            clock: self.clock.clone(),
            token: RefCell::default(),
            sequence: Cell::new(0),
            pending: RefCell::default(),
            attachment: RefCell::default(),
            jobs: RefCell::default(),
        });
        let weak = Rc::downgrade(&state);
        self.identity_bus
            .borrow_mut()
            .push(Box::new(move |revoked| {
                if let Some(state) = weak.upgrade() {
                    let attachment = state.attachment.borrow().clone();
                    if let Some((id, generation)) = attachment
                        && revoked.contains(&id)
                    {
                        state.attachment.borrow_mut().take();
                        state.input(Input::Disconnected {
                            generation,
                            reason: Disconnect::AuthorityEnded,
                        });
                    }
                }
            }));
        state.input(Input::Start);
        Client { state }
    }
}
impl<P: Provider<Context = Context, Output = Response> + 'static> Client<P> {
    pub fn snapshot(&self) -> Snapshot {
        self.state.core.borrow().snapshot()
    }
    pub fn command(
        &self,
        key: &str,
        payload: Option<Value>,
    ) -> impl std::future::Future<Output = Outcome> + 'static {
        let id = self.state.sequence.get() + 1;
        self.state.sequence.set(id);
        let (send, receive) = oneshot::channel();
        self.state.pending.borrow_mut().insert(id, send);
        self.state.input(Input::Command {
            id,
            key: key.into(),
            payload,
        });
        async {
            receive.await.unwrap_or_else(|_| {
                Err(Error::UnavailableError {
                    message: "Client closed".into(),
                })
            })
        }
    }
    pub fn close(&self) {
        self.state.input(Input::Close);
    }
}
impl<P: Provider<Context = Context, Output = Response> + 'static> State<P> {
    fn input(self: &Rc<Self>, input: Input) {
        if matches!(input, Input::Close) {
            self.cancel(false);
        }
        let mut actions = Vec::new();
        self.core.borrow_mut().update(input, &mut actions);
        for action in actions {
            match action {
                Action::Complete { id, outcome } => {
                    if let Some(send) = self.pending.borrow_mut().remove(&id) {
                        let _ = send.send(outcome);
                    }
                    continue;
                }
                Action::Disconnect => {
                    self.cancel(true);
                    self.attachment.borrow_mut().take();
                    continue;
                }
                Action::Reload => {
                    self.input(Input::Close);
                    continue;
                }
                _ => {}
            }
            let socket = !matches!(action, Action::Request { .. });
            let (abort, registration) = AbortHandle::new_pair();
            self.jobs.borrow_mut().retain(|(_, job)| !job.is_aborted());
            self.jobs.borrow_mut().push((socket, abort.clone()));
            let state = self.clone();
            self.endpoint
                .spawner
                .spawn_local(async move {
                    let _ = Abortable::new(state.action(action), registration).await;
                    abort.abort();
                })
                .expect("live memory executor");
        }
    }
    fn cancel(&self, socket_only: bool) {
        self.jobs.borrow_mut().retain(|(socket, job)| {
            if !socket_only || *socket {
                job.abort();
                false
            } else {
                !job.is_aborted()
            }
        });
    }
    async fn action(self: Rc<Self>, action: Action) {
        match action {
            Action::Request {
                generation,
                invocation,
            } => self.deliver(generation, invocation, false).await,
            Action::Send {
                generation,
                invocation,
            } => self.deliver(generation, invocation, true).await,
            Action::Connect { generation } => {
                let invocation = Invocation {
                    operation_id: "attach".into(),
                    key: "identity.fetch".into(),
                    payload: None,
                    traceparent: None,
                };
                let mut call = self.endpoint.submit(
                    invocation,
                    Context {
                        token: self.token.borrow().clone(),
                        now: self.clock.now(),
                    },
                );
                while let Some(event) = call.next().await {
                    if let Event::Completed(reply) = event {
                        if let Some(session) = reply.session.filter(|_| reply.outcome.is_ok()) {
                            *self.attachment.borrow_mut() =
                                Some((session.session_id.clone(), generation));
                            self.input(Input::Event {
                                generation,
                                event: Ok(ConnectionEvent::Attached),
                            });
                            let sleep = self
                                .clock
                                .sleep(session.expires_at.saturating_sub(self.clock.now()));
                            let state = self.clone();
                            let (abort, registration) = AbortHandle::new_pair();
                            self.jobs.borrow_mut().push((true, abort.clone()));
                            self.endpoint
                                .spawner
                                .spawn_local(async move {
                                    let _ = Abortable::new(
                                        async move {
                                            sleep.await;
                                            let attachment = state.attachment.borrow().clone();
                                            if attachment == Some((session.session_id, generation))
                                            {
                                                state.attachment.borrow_mut().take();
                                                state.input(Input::Disconnected {
                                                    generation,
                                                    reason: Disconnect::AuthorityEnded,
                                                });
                                            }
                                        },
                                        registration,
                                    )
                                    .await;
                                    abort.abort();
                                })
                                .expect("live memory executor");
                        } else {
                            self.input(Input::Disconnected {
                                generation,
                                reason: Disconnect::AuthorityEnded,
                            });
                        }
                    }
                }
            }
            Action::Retry {
                generation,
                milliseconds,
            } => {
                self.clock.sleep(milliseconds as u64).await;
                self.input(Input::Retry { generation });
            }
            Action::ReadDeadline { generation, id } => {
                self.clock.sleep(5_000).await;
                self.input(Input::ReadTimeout { generation, id });
            }
            Action::Complete { .. } | Action::Reload | Action::Disconnect => {
                unreachable!("synchronous actions handled at input")
            }
        }
    }
    async fn deliver(self: Rc<Self>, generation: u64, invocation: Invocation, stream: bool) {
        let id = invocation.operation_id.clone();
        let mut call = self.endpoint.submit(
            invocation,
            Context {
                token: self.token.borrow().clone(),
                now: self.clock.now(),
            },
        );
        while let Some(event) = call.next().await {
            match event {
                Event::Accepted if stream => self.input(Input::Event {
                    generation,
                    event: Ok(ConnectionEvent::Accepted { id: id.clone() }),
                }),
                Event::Accepted => {}
                Event::Completed(reply) => {
                    if let Some(token) = reply.token {
                        *self.token.borrow_mut() = token;
                    }
                    if stream {
                        if matches!(reply.outcome, Err(Error::IdentityRequiredError { .. })) {
                            self.input(Input::Disconnected {
                                generation,
                                reason: Disconnect::AuthorityEnded,
                            });
                        } else {
                            self.input(Input::Event {
                                generation,
                                event: Ok(ConnectionEvent::Completed {
                                    id: id.clone(),
                                    outcome: reply.outcome,
                                }),
                            });
                        }
                    } else {
                        self.input(Input::Completed {
                            generation,
                            id: id.clone(),
                            result: Ok(reply.outcome),
                        });
                    }
                }
            }
        }
    }
}
