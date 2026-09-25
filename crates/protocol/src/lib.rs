//! The small part of Snap's protocol needed by Healthy. No platform dependencies.
#![no_std]

extern crate alloc;

use alloc::string::String;
use serde::{Deserialize, Serialize};

pub use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
pub struct Operation {
    pub key: &'static str,
}

/// A statically composed provider. Invocation creates an owned continuation;
/// the host owns polling, admission, cancellation, and external work execution.
/// Context and output belong to the capability/composition, not the scheduler.
/// Continuations are local: neither providers nor futures must cross threads.
/// Hosts move owned external-work requests across threads when needed instead.
pub trait Provider {
    type Context;
    type Output;
    fn operations(&self) -> impl Iterator<Item = Operation>;
    fn invoke(
        &mut self,
        invocation: Invocation,
        context: Self::Context,
    ) -> impl core::future::Future<Output = Self::Output> + 'static;
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
    Completed { id: String, outcome: Outcome },
    Notification { key: String },
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Disconnect {
    Interrupted,
    AuthorityEnded,
    BuildChanged,
}
