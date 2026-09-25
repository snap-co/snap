//! In-process execution platform. No sockets, files, wall clock, or wire encoding.
//! The owner loads a provider, queues SDK work, and explicitly drives the executor.
//! This is a healthy local platform, not a distributed deterministic simulator.
pub mod fixture;
pub mod identity;
pub mod store;

use futures::{
    StreamExt,
    channel::{mpsc, oneshot},
    executor::{LocalPool, LocalSpawner},
    task::LocalSpawnExt,
};
use snap_protocol::{Invocation, Provider, Rejection};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    rc::Rc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Accepted,
    Completed,
    Rejected,
}
#[derive(Clone, Debug)]
pub struct Trace {
    pub id: String,
    pub operation: String,
    pub phase: Phase,
}
pub enum Event<T> {
    Accepted,
    Completed(T),
}
pub type Call<T> = mpsc::UnboundedReceiver<Event<T>>;
type Projection<T> = Rc<RefCell<Option<Rc<dyn Fn(&T)>>>>;

/// Virtual monotonic time. No timer wakes until the owner advances it.
#[derive(Clone, Default)]
pub struct Clock(Rc<RefCell<ClockState>>);
#[derive(Default)]
struct ClockState {
    now: u64,
    timers: Vec<(u64, oneshot::Sender<()>)>,
}
impl Clock {
    pub fn now(&self) -> u64 {
        self.0.borrow().now
    }
    pub fn advance(&self, milliseconds: u64) {
        let mut state = self.0.borrow_mut();
        state.now = state
            .now
            .checked_add(milliseconds)
            .expect("clock exhausted");
        let now = state.now;
        let mut pending = Vec::new();
        for (at, signal) in state.timers.drain(..) {
            if at <= now {
                let _ = signal.send(());
            } else if !signal.is_canceled() {
                pending.push((at, signal));
            }
        }
        state.timers = pending;
    }
    pub fn sleep(&self, milliseconds: u64) -> impl Future<Output = ()> + 'static {
        let (send, receive) = oneshot::channel();
        let mut state = self.0.borrow_mut();
        let at = state
            .now
            .checked_add(milliseconds)
            .expect("clock exhausted");
        state.timers.push((at, send));
        async {
            let _ = receive.await;
        }
    }
}

pub struct Rig<P: Provider> {
    pool: LocalPool,
    pub clock: Clock,
    endpoint: Endpoint<P>,
    identity_bus: identity::Bus,
}
pub(crate) struct Endpoint<P: Provider> {
    provider: Rc<RefCell<P>>,
    spawner: LocalSpawner,
    trace: Rc<RefCell<Vec<Trace>>>,
    active: Rc<Cell<usize>>,
    capacity: usize,
    project: Projection<P::Output>,
}
impl<P: Provider> Clone for Endpoint<P> {
    fn clone(&self) -> Self {
        Self {
            provider: self.provider.clone(),
            spawner: self.spawner.clone(),
            trace: self.trace.clone(),
            active: self.active.clone(),
            capacity: self.capacity,
            project: self.project.clone(),
        }
    }
}
struct Permit(Rc<Cell<usize>>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl<P: Provider + 'static> Rig<P> {
    pub fn new(provider: P) -> Self {
        Self::with_capacity(provider, 64)
    }
    pub fn with_capacity(provider: P, capacity: usize) -> Self {
        let pool = LocalPool::new();
        let endpoint = Endpoint {
            provider: Rc::new(RefCell::new(provider)),
            spawner: pool.spawner(),
            trace: Rc::default(),
            active: Rc::default(),
            capacity,
            project: Rc::default(),
        };
        Self {
            pool,
            endpoint,
            clock: Clock::default(),
            identity_bus: Rc::default(),
        }
    }
    pub fn submit(&self, invocation: Invocation, context: P::Context) -> Call<P::Output>
    where
        P::Context: 'static,
    {
        self.endpoint.submit(invocation, context)
    }
    pub fn run_until_stalled(&mut self) {
        self.pool.run_until_stalled();
    }
    /// Drive a known-completing scenario. For intentionally held work use
    /// run_until_stalled, inspect events, then release the dependency explicitly.
    pub fn run<F: Future>(&mut self, future: F) -> F::Output {
        self.pool.run_until(future)
    }
    pub fn complete(&mut self, mut call: Call<P::Output>) -> P::Output {
        self.run(async move {
            while let Some(event) = call.next().await {
                if let Event::Completed(output) = event {
                    return output;
                }
            }
            panic!("memory host dropped completion")
        })
    }
    pub fn trace(&self) -> Vec<Trace> {
        self.endpoint.trace.borrow().clone()
    }
    pub fn active(&self) -> usize {
        self.endpoint.active.get()
    }
}
impl<P: Provider + 'static> Endpoint<P> {
    fn submit(&self, invocation: Invocation, context: P::Context) -> Call<P::Output>
    where
        P::Context: 'static,
    {
        let (send, receive) = mpsc::unbounded();
        let endpoint = self.clone();
        self.spawner
            .spawn_local(async move {
                let record = |phase| {
                    endpoint.trace.borrow_mut().push(Trace {
                        id: invocation.operation_id.clone(),
                        operation: invocation.key.clone(),
                        phase,
                    })
                };
                if endpoint.active.get() >= endpoint.capacity {
                    record(Phase::Rejected);
                    let _ = send.unbounded_send(Event::Completed(P::Output::rejected(
                        snap_protocol::Error::UnavailableError {
                            message: "Host is at capacity".into(),
                        },
                    )));
                    return;
                }
                endpoint.active.set(endpoint.active.get() + 1);
                let _permit = Permit(endpoint.active.clone());
                let preparation = snap_protocol::dispatch(
                    &mut *endpoint.provider.borrow_mut(),
                    invocation.clone(),
                    context,
                );
                let output = match preparation.await {
                    Ok(work) => {
                        let future = work.start(|| {
                            record(Phase::Accepted);
                            let _ = send.unbounded_send(Event::Accepted);
                        });
                        let output = future.await;
                        record(Phase::Completed);
                        output
                    }
                    Err(refusal) => {
                        record(Phase::Rejected);
                        refusal.into_output()
                    }
                };
                // Observation loss never cancels accepted work.
                if let Some(project) = endpoint.project.borrow().as_ref() {
                    project(&output);
                }
                let _ = send.unbounded_send(Event::Completed(output));
            })
            .expect("live memory executor");
        receive
    }
}
