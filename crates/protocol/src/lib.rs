//! Carrier-independent invocation, admission and completion contracts.
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};
use core::{future::Future, pin::Pin};
use serde::{Deserialize, Serialize};

pub use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
pub struct Operation {
    pub key: &'static str,
    pub input: fn(&Option<Value>) -> Result<(), Error>,
    pub identity: IdentityPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPolicy {
    Optional,
    Required,
    Anonymous,
}
impl IdentityPolicy {
    /// `identified` must come from verified authority, never a caller's claim.
    pub fn check(self, identified: bool) -> Result<(), Error> {
        match (self, identified) {
            (Self::Required, false) => Err(Error::IdentityRequiredError {
                message: "Identity required".into(),
            }),
            (Self::Anonymous, true) => Err(Error::IdentityForbiddenError {
                message: "Identity forbidden".into(),
            }),
            _ => Ok(()),
        }
    }
}
impl Operation {
    pub const fn new(
        key: &'static str,
        input: fn(&Option<Value>) -> Result<(), Error>,
        identity: IdentityPolicy,
    ) -> Self {
        Self {
            key,
            input,
            identity,
        }
    }
}

/// An explicitly unconstrained schema, useful for echo/diagnostic operations.
pub fn any_input(_: &Option<Value>) -> Result<(), Error> {
    Ok(())
}

pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// An operation-selected admission guard. It returns owned work so the dispatcher
/// never borrows a provider across suspension. Guards must defer external work
/// until polled. Context is supplied by trusted composition, not wire payload.
pub type Guard<C> = fn(&Invocation, &C) -> LocalFuture<Result<GuardLease, Error>>;

/// Holds a guard's reservation until rejection, completion, or execution shutdown.
/// Put rollback/release in the owned value's Drop implementation. Irreversible
/// guard side effects are not rolled back by Transport.
pub struct GuardLease(Box<dyn core::any::Any>);
impl GuardLease {
    pub fn new(value: impl core::any::Any) -> Self {
        Self(Box::new(value))
    }
}

/// Prepared work cannot enter its handler until the dispatcher emits acceptance.
/// Guard-owned reservations captured here are released if preparation is dropped.
pub struct Accepted<T> {
    start: Box<dyn FnOnce() -> LocalFuture<T>>,
}
impl<T: 'static> Accepted<T> {
    pub fn new<F: Future<Output = T> + 'static>(start: impl FnOnce() -> F + 'static) -> Self {
        Self {
            start: Box::new(move || Box::pin(start())),
        }
    }
    pub fn start(self, accepted: impl FnOnce()) -> LocalFuture<T> {
        accepted();
        (self.start)()
    }
    pub fn map<U: 'static>(self, project: impl FnOnce(T) -> U + 'static) -> Accepted<U> {
        Accepted::new(move || async move { project((self.start)().await) })
    }
    fn retain(self, leases: Vec<GuardLease>) -> Self {
        Self::new(move || {
            let future = (self.start)();
            async move {
                let result = future.await;
                // Explicit drop also ensures reservations outlive the handler.
                for lease in leases {
                    drop(lease.0);
                }
                result
            }
        })
    }
}

pub trait Rejection {
    fn rejected(error: Error) -> Self;
}
impl Rejection for Outcome {
    fn rejected(error: Error) -> Self {
        Err(error)
    }
}

/// Admission may reject with capability-owned effects, such as clearing a stale
/// session credential. These are projected without acknowledging or entering a
/// handler. Ordinary schema/guard errors need no capability metadata.
#[derive(Debug)]
pub enum Refusal<T> {
    Error(Error),
    Reply(T),
}
impl<T> From<Error> for Refusal<T> {
    fn from(error: Error) -> Self {
        Self::Error(error)
    }
}
impl<T: Rejection> Refusal<T> {
    pub fn into_output(self) -> T {
        match self {
            Self::Error(error) => T::rejected(error),
            Self::Reply(reply) => reply,
        }
    }
}
pub type Admission<T> = Result<Accepted<T>, Refusal<T>>;

/// Compose both successful preparation and rejection effects through one adapter.
pub fn project<T: Rejection + 'static, U: 'static>(
    admission: Admission<T>,
    project: impl FnOnce(T) -> U + 'static,
) -> Admission<U> {
    match admission {
        Ok(work) => Ok(work.map(project)),
        Err(refusal) => Err(Refusal::Reply(project(refusal.into_output()))),
    }
}

/// A statically composed provider. Shared dispatch validates input and runs guards;
/// the host owns capacity, polling, cancellation, and external work execution.
/// Context and output belong to the capability/composition, not the scheduler.
/// Continuations are local: neither providers nor futures must cross threads.
/// Hosts move owned external-work requests across threads when needed instead.
pub trait Provider {
    type Context;
    type Output: Rejection + 'static;
    fn operations(&self) -> impl Iterator<Item = Operation>;
    /// Additional guards selected by operation. Shared dispatch polls them in
    /// declaration order after schema validation and before provider preparation.
    /// Provider preparation resolves capability-specific authority and may reject.
    fn guards(&self, _key: &str) -> impl Iterator<Item = Guard<Self::Context>> {
        core::iter::empty()
    }
    /// Resolve operation guards after schema validation. This must not run the
    /// handler. Returned work owns all context needed after acceptance.
    fn prepare(
        &mut self,
        invocation: Invocation,
        context: Self::Context,
    ) -> impl Future<Output = Admission<Self::Output>> + 'static;

    /// In-process convenience for callers that do not observe acknowledgements.
    fn invoke(
        &mut self,
        invocation: Invocation,
        context: Self::Context,
    ) -> impl Future<Output = Self::Output> + 'static {
        let pending = dispatch(self, invocation, context);
        async move {
            match pending.await {
                Ok(work) => work.start(|| {}).await,
                Err(refusal) => refusal.into_output(),
            }
        }
    }
}

/// All carriers enter here. Schema validation precedes provider guards; neither
/// phase invokes the handler. Hosts reserve bounded execution capacity before
/// dispatch and retain it through completion. Acceptance is not durable execution.
pub fn dispatch<P: Provider + ?Sized>(
    provider: &mut P,
    invocation: Invocation,
    context: P::Context,
) -> LocalFuture<Admission<P::Output>> {
    let operation = provider.operations().find(|op| op.key == invocation.key);
    let validation = operation
        .ok_or_else(|| Error::ContractViolationError {
            message: alloc::format!("Unknown key: {}", invocation.key),
        })
        .and_then(|op| (op.input)(&invocation.payload));
    match validation {
        Err(error) => Box::pin(async { Err(error.into()) }),
        Ok(()) => {
            let guards: Vec<_> = provider
                .guards(&invocation.key)
                .map(|guard| guard(&invocation, &context))
                .collect();
            let preparation = provider.prepare(invocation, context);
            Box::pin(async move {
                let mut leases = Vec::new();
                for guard in guards {
                    leases.push(guard.await?);
                }
                Ok(preparation.await?.retain(leases))
            })
        }
    }
}

/// Carrier framing has already been removed. Missing payload differs from JSON null.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    pub operation_id: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
}

/// Only errors exercised by this slice. Extend alongside the behavior that needs them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum Error {
    InvalidInputError { message: String },
    ContractViolationError { message: String },
    UnavailableError { message: String },
    IdentityRequiredError { message: String },
    IdentityForbiddenError { message: String },
    OperationError { failure: Value },
    IndeterminateError { admission: String, message: String },
}

pub type Outcome = Result<Value, Error>;

pub enum ConnectionEvent {
    Attached,
    Accepted { id: String },
    Completed { id: String, outcome: Outcome },
    Notification { key: String },
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Disconnect {
    Interrupted,
    AuthorityEnded,
    BuildChanged,
}
