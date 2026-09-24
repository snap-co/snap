//! Doctor's liveness Query. Operational checks can follow when a controller needs them.

use snap_protocol::{Error, Operation, json};

use crate::transport::Handler;

pub fn up<State>() -> Handler<State> {
    Handler {
        operation: Operation { key: "health.up" },
        run: |_, payload| {
            if payload.is_some() {
                return Err(Error::InvalidInputError {
                    message: "health.up expects no input".into(),
                });
            }
            Ok(json!({ "status": "OK" }))
        },
    }
}
