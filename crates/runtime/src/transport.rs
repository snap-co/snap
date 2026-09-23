//! Shared dispatch behind the normalized host input. No HTTP or socket types.

use alloc::{format, vec::Vec};
use snap_protocol::{Error, Operation, Outcome, Value};

use crate::{Action, Input, Module};

pub struct Handler<State> {
    pub operation: Operation,
    pub run: fn(&mut State, Option<Value>) -> Outcome,
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
        } = input;
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
