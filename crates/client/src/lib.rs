//! IO-free client behavior. A platform carries invocations and returns wire observations.
#![no_std]

extern crate alloc;

pub mod application;

use alloc::{format, string::String};
use serde::{Deserialize, Serialize};
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    key: String,
    target: Option<String>,
    payload: Option<Value>,
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

    pub fn complete(&self, wire: &str) -> Result<HealthReport, Error> {
        let event: Event = serde_json::from_str(wire)
            .map_err(|_| invalid("Invalid Transport completion Event"))?;
        if event.key != "transport.complete"
            || event.target.as_deref() != Some(self.invocation.operation_id.as_str())
        {
            return Err(invalid("Invalid Transport completion Event"));
        }
        let payload = event
            .payload
            .ok_or_else(|| invalid("Missing Transport result"))?;
        match payload.get("ok").and_then(Value::as_bool) {
            Some(true) => {
                if payload
                    .get("payload")
                    .and_then(|report| report.get("status"))
                    .and_then(Value::as_str)
                    != Some("OK")
                {
                    return Err(invalid("Invalid health.up result"));
                }
                Ok(HealthReport { status: "OK" })
            }
            Some(false) => {
                let error = payload
                    .get("error")
                    .cloned()
                    .ok_or_else(|| invalid("Missing Transport error"))?;
                Err(serde_json::from_value(error)
                    .map_err(|_| invalid("Invalid Transport error"))?)
            }
            None => Err(invalid("Invalid Transport result outcome")),
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::ContractViolationError {
        message: message.into(),
    }
}
