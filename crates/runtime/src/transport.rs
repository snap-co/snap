//! Shared dispatch behind the normalized host input. No HTTP or socket types.

use alloc::{format, string::String, vec::Vec};
use snap_protocol::{Error, Invocation, Lane, Operation, Outcome, Value};

use crate::{Action, Input, Module};

pub struct Handler<State> {
    pub operation: Operation,
    pub run: fn(&mut State, Option<Value>) -> Outcome,
}

/// Read-only Message admission for one logical lifetime. Reattachment currently
/// creates a new lifetime. Accepted IDs are never executed twice in that lifetime;
/// callers receive Indeterminate rather than an invented cached completion.
pub struct Connection {
    epoch: String,
    sequence: u64,
}

impl Connection {
    pub fn new(epoch: String) -> Self {
        Self { epoch, sequence: 0 }
    }
    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    pub fn admit(&mut self, invocation: &Invocation, lane: Option<Lane>) -> Result<(), Error> {
        if lane != Some(Lane::Message) {
            return Err(Error::ContractViolationError {
                message: "Operation is not registered for this carrier".into(),
            });
        }
        let parsed = invocation
            .operation_id
            .rsplit_once(':')
            .and_then(|(epoch, sequence)| {
                sequence
                    .split('#')
                    .next()?
                    .parse::<u64>()
                    .ok()
                    .map(|n| (epoch, n))
            });
        match parsed {
            Some((epoch, n))
                if epoch == self.epoch
                    && n > 0
                    && n <= 9_007_199_254_740_991
                    && n == self.sequence + 1 =>
            {
                self.sequence = n;
                Ok(())
            }
            Some((epoch, n)) if epoch == self.epoch && n > 0 && n <= self.sequence => {
                Err(Error::IndeterminateError {
                    admission: "accepted".into(),
                    message: "Invocation was already received".into(),
                })
            }
            _ => Err(Error::IndeterminateError {
                admission: "unknown".into(),
                message: "Invocation is outside the receive fence".into(),
            }),
        }
    }
}

/// Resident application state lives here. The host need not know its shape.
pub struct Transport<State> {
    state: State,
    handlers: Vec<Handler<State>>,
}

impl<State> Transport<State> {
    pub fn new(state: State, handlers: Vec<Handler<State>>) -> Result<Self, Error> {
        for (index, handler) in handlers.iter().enumerate() {
            if handler.operation.key.split('.').any(|segment| {
                segment.is_empty()
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            }) {
                return Err(Error::ContractViolationError {
                    message: format!("Invalid operation key: {}", handler.operation.key),
                });
            }
            if handlers[..index]
                .iter()
                .any(|other| other.operation.key == handler.operation.key)
            {
                return Err(Error::ContractViolationError {
                    message: format!("Duplicate operation key: {}", handler.operation.key),
                });
            }
        }
        Ok(Self { state, handlers })
    }
}

impl<State> Module for Transport<State> {
    fn operations(&self) -> impl Iterator<Item = Operation> {
        self.handlers.iter().map(|handler| handler.operation)
    }

    fn update(&mut self, input: Input, actions: &mut Vec<Action>) {
        let Input::Invocation {
            delivery,
            invocation,
            ..
        } = input
        else {
            return;
        };
        let outcome = match self
            .handlers
            .iter()
            .find(|handler| handler.operation.key == invocation.key)
        {
            Some(handler) => (handler.run)(&mut self.state, invocation.payload),
            None => Err(Error::ContractViolationError {
                message: format!("Unknown key: {}", invocation.key),
            }),
        };
        actions.push(Action::Complete {
            delivery,
            operation_id: invocation.operation_id,
            outcome,
        });
    }
}
