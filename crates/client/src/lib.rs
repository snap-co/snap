//! IO-free client behavior. A platform carries invocations and returns wire observations.
#![no_std]

extern crate alloc;

pub mod application;
pub mod identity;

use alloc::format;
use serde::Serialize;
use snap_protocol::{Error, Invocation, Value};

#[derive(Default)]
pub struct Client {
    sequence: u64,
}

#[derive(Debug, Serialize)]
pub struct HealthReport {
    pub status: &'static str,
}

/// One owned query. Dropping it releases observation without retaining a pending map.
pub struct HealthQuery {
    invocation: Invocation,
}

impl Client {
    pub fn health_up(&mut self) -> Result<HealthQuery, Error> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("Client operation sequence exhausted"))?;
        Ok(HealthQuery {
            invocation: Invocation {
                operation_id: format!("query-{}", self.sequence),
                key: "health.up".into(),
                payload: None,
                traceparent: None,
            },
        })
    }
}

impl HealthQuery {
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }

    pub fn complete(&self, payload: &Value) -> Result<HealthReport, Error> {
        if payload.get("status").and_then(Value::as_str) != Some("OK") {
            return Err(invalid("Invalid health.up result"));
        }
        Ok(HealthReport { status: "OK" })
    }
}

fn invalid(message: &str) -> Error {
    Error::ContractViolationError {
        message: message.into(),
    }
}
