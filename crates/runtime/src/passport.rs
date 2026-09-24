//! Password policy and session workflows. Every turn is IO-free.
//!
//! The host executes Work and returns exactly one completion for its Delivery.
//! Accepted writes finish even if the caller disconnects. No mutation is retried
//! implicitly. Enroll atomically claims a unique (kind, normalized email) and its
//! first session; CreateSession must reference an existing credential. Revoke
//! checks the caller's live session and identity in the same storage transaction.
use crate::{Action, Delivery, Input, Module};
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};
use snap_protocol::{
    Error, Invocation, Lane, Operation, Outcome, Value,
    identity::{Credential, Release, Session},
    json,
};

pub const SESSION_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Default)]
pub struct Context {
    pub token: Option<String>,
    /// Host time in Unix milliseconds. Session expiry is checked on every request.
    pub now: u64,
}

pub enum Work {
    Resolve {
        token: String,
        now: u64,
    },
    Credential {
        kind: String,
        email: String,
    },
    Hash {
        password: String,
    },
    Verify {
        password: String,
        hash: String,
    },
    Enroll {
        kind: String,
        email: String,
        hash: String,
        now: u64,
    },
    CreateSession {
        credential: Credential,
        now: u64,
    },
    Credentials {
        session: Session,
        now: u64,
    },
    Sessions {
        session: Session,
        now: u64,
    },
    Revoke {
        session: Session,
        scope: Release,
        now: u64,
    },
}

pub enum Result {
    Session(Option<Session>),
    Credential(Option<Credential>),
    Hash(String),
    Verified(bool),
    Created { token: String },
    Data(Value),
    Revoked(Vec<String>),
}

enum Stage {
    Resolve,
    Hash,
    Credential,
    Verify(Credential),
    Create,
    Read,
    Revoke { clear: bool },
}
struct Pending {
    invocation: Invocation,
    context: Context,
    stage: Stage,
    email: String,
    password: String,
}

/// The application selects its identity kind and account-registration operation.
/// Passwords are consumed by work actions and never retained in public snapshots.
pub struct Passport {
    kind: &'static str,
    register: &'static str,
    pending: BTreeMap<Delivery, Pending>,
}

impl Passport {
    pub fn new(kind: &'static str, register: &'static str) -> Self {
        Self {
            kind,
            register,
            pending: BTreeMap::new(),
        }
    }

    fn dispatch(
        &self,
        delivery: Delivery,
        p: &mut Pending,
        session: Option<Session>,
        actions: &mut Vec<Action>,
    ) -> core::result::Result<Option<Work>, Error> {
        let key = p.invocation.key.as_str();
        if let Some(session) = &session {
            actions.push(Action::Resolved {
                delivery,
                session: session.clone(),
            });
        } else if p.context.token.is_some() {
            actions.push(Action::Session {
                delivery,
                token: None,
            });
        }
        if key == "identity.fetch" {
            void(&p.invocation.payload)?;
            finish(
                delivery,
                p,
                Ok(json!({"identityId": session.map(|s| s.identity_id)})),
                actions,
            );
            return Ok(None);
        }
        if key == self.register || key == "identity.password.acquire" {
            if key != self.register && session.is_some() {
                return Err(Error::IdentityForbiddenError {
                    message: "Identity forbidden".into(),
                });
            }
            let value = p.invocation.payload.as_ref().ok_or_else(invalid)?;
            p.email = value
                .get("email")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?
                .trim()
                .to_lowercase();
            p.password = value
                .get("password")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?
                .into();
            if key == self.register {
                // Match Identity.Password's JS string-length bounds.
                if !(8..=256).contains(&p.password.encode_utf16().count()) {
                    return Err(invalid());
                }
                p.stage = Stage::Hash;
                return Ok(Some(Work::Hash {
                    password: core::mem::take(&mut p.password),
                }));
            }
            if value.get("kind").and_then(Value::as_str) != Some(self.kind) {
                return Err(domain("InvalidCredentialError", "Invalid credential"));
            }
            p.stage = Stage::Credential;
            return Ok(Some(Work::Credential {
                kind: self.kind.into(),
                email: p.email.clone(),
            }));
        }
        let session = session.ok_or_else(|| Error::IdentityRequiredError {
            message: "Identity required".into(),
        })?;
        match key {
            "identity.credentials" | "identity.sessions" => {
                void(&p.invocation.payload)?;
                p.stage = Stage::Read;
                Ok(Some(if key == "identity.credentials" {
                    Work::Credentials {
                        session,
                        now: p.context.now,
                    }
                } else {
                    Work::Sessions {
                        session,
                        now: p.context.now,
                    }
                }))
            }
            "identity.release" => {
                let scope: Release =
                    serde_json::from_value(p.invocation.payload.clone().ok_or_else(invalid)?)
                        .map_err(|_| invalid())?;
                let clear = match &scope {
                    Release::Current | Release::All => true,
                    Release::Session { session_id } => *session_id == session.session_id,
                    Release::Others => false,
                };
                p.stage = Stage::Revoke { clear };
                Ok(Some(Work::Revoke {
                    session,
                    scope,
                    now: p.context.now,
                }))
            }
            _ => Err(Error::ContractViolationError {
                message: "Unknown operation".into(),
            }),
        }
    }
}

impl Module for Passport {
    fn operations(&self) -> impl Iterator<Item = Operation> {
        [
            Operation {
                key: self.register,
                lane: Lane::Submit,
            },
            Operation {
                key: "identity.fetch",
                lane: Lane::Query,
            },
            Operation {
                key: "identity.password.acquire",
                lane: Lane::Submit,
            },
            Operation {
                key: "identity.release",
                lane: Lane::Submit,
            },
            Operation {
                key: "identity.credentials",
                lane: Lane::Message,
            },
            Operation {
                key: "identity.sessions",
                lane: Lane::Message,
            },
        ]
        .into_iter()
    }

    fn update(&mut self, input: Input, actions: &mut Vec<Action>) {
        let (delivery, mut p, result) = match input {
            Input::Invocation {
                delivery,
                invocation,
                context,
            } => {
                let p = Pending {
                    invocation,
                    context,
                    stage: Stage::Resolve,
                    email: String::new(),
                    password: String::new(),
                };
                if let Some(token) = &p.context.token {
                    actions.push(Action::Work {
                        delivery,
                        work: Work::Resolve {
                            token: token.clone(),
                            now: p.context.now,
                        },
                    });
                    self.pending.insert(delivery, p);
                    return;
                }
                (delivery, p, Ok(Result::Session(None)))
            }
            Input::Completed { delivery, result } => {
                let Some(p) = self.pending.remove(&delivery) else {
                    return;
                };
                (delivery, p, result)
            }
        };
        let next = (|| {
            let result = result?;
            match (&p.stage, result) {
                (Stage::Resolve, Result::Session(session)) => {
                    self.dispatch(delivery, &mut p, session, actions)
                }
                (Stage::Hash, Result::Hash(hash)) => {
                    p.stage = Stage::Create;
                    Ok(Some(Work::Enroll {
                        kind: self.kind.into(),
                        email: p.email.clone(),
                        hash,
                        now: p.context.now,
                    }))
                }
                (Stage::Credential, Result::Credential(credential)) => {
                    let credential = credential
                        .ok_or_else(|| domain("InvalidCredentialError", "Invalid credential"))?;
                    let work = Work::Verify {
                        password: core::mem::take(&mut p.password),
                        hash: credential.hash.clone(),
                    };
                    p.stage = Stage::Verify(credential);
                    Ok(Some(work))
                }
                (Stage::Verify(credential), Result::Verified(valid)) => {
                    if !valid {
                        return Err(domain("InvalidCredentialError", "Invalid credential"));
                    }
                    let work = Work::CreateSession {
                        credential: credential.clone(),
                        now: p.context.now,
                    };
                    p.stage = Stage::Create;
                    Ok(Some(work))
                }
                (Stage::Create, Result::Created { token }) => {
                    actions.push(Action::Session {
                        delivery,
                        token: Some(token),
                    });
                    finish(
                        delivery,
                        &p,
                        Ok(if p.invocation.key == self.register {
                            Value::Null
                        } else {
                            json!({"_tag":"Approved"})
                        }),
                        actions,
                    );
                    Ok(None)
                }
                (Stage::Read, Result::Data(value)) => {
                    finish(delivery, &p, Ok(value), actions);
                    Ok(None)
                }
                (Stage::Revoke { clear }, Result::Revoked(sessions)) => {
                    if *clear {
                        actions.push(Action::Session {
                            delivery,
                            token: None,
                        });
                    }
                    actions.push(Action::Revoke { sessions });
                    finish(delivery, &p, Ok(Value::Null), actions);
                    Ok(None)
                }
                _ => Err(domain("IdentityUnavailable", "Unexpected work completion")),
            }
        })();
        match next {
            Ok(Some(work)) => {
                self.pending.insert(delivery, p);
                actions.push(Action::Work { delivery, work });
            }
            Ok(None) => {}
            Err(error) => finish(delivery, &p, Err(error), actions),
        }
    }
}

fn finish(delivery: Delivery, p: &Pending, outcome: Outcome, actions: &mut Vec<Action>) {
    if matches!(outcome, Ok(Value::Null)) {
        actions.push(Action::CompleteEmpty {
            delivery,
            operation_id: p.invocation.operation_id.clone(),
        });
    } else {
        actions.push(Action::Complete {
            delivery,
            operation_id: p.invocation.operation_id.clone(),
            outcome,
        });
    }
}
fn invalid() -> Error {
    Error::InvalidInputError {
        message: "Invalid input".into(),
    }
}
fn void(value: &Option<Value>) -> core::result::Result<(), Error> {
    if value.is_none() || matches!(value, Some(Value::Null)) {
        Ok(())
    } else {
        Err(invalid())
    }
}
pub fn domain(tag: &str, message: &str) -> Error {
    Error::OperationError {
        failure: json!({"_tag":tag,"message":message.to_string()}),
    }
}
