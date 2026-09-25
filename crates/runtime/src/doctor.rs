//! Doctor's liveness Query. Operational checks can follow when a controller needs them.

use snap_protocol::{Error, Operation, json};

use crate::transport::Handler;

pub fn up<State>() -> Handler<State> {
    Handler {
        operation: Operation::new(
            "health.up",
            |payload| {
                if payload.is_none() {
                    Ok(())
                } else {
                    Err(Error::InvalidInputError {
                        message: "health.up expects no input".into(),
                    })
                }
            },
            snap_protocol::IdentityPolicy::Optional,
        ),
        run: |_, _| Ok(json!({ "status": "OK" })),
    }
}
