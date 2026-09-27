//! Identity's portable operation dispatch. Parsing precedes Store entry. The host
//! publishes execute's output only after the enclosing durable transaction commits.
use crate::{Crypto, Identity};
use alloc::string::String;
use snap_store::Transaction;
use snap_transport::{Error, Invocation, Value, json};

pub enum Operation {
    Enroll { email: String, password: String },
    Login { email: String, password: String },
    Current { bearer: String },
    Logout { bearer: String },
}
impl Operation {
    /// None lets composition dispatch to another capability. Unknown Identity
    /// operations fail here, so credentials never enter the calculator/debug trace.
    pub fn parse(invocation: &Invocation, bearer: Option<&str>) -> Result<Option<Self>, Error> {
        let key = invocation.operation.as_str();
        if !key.starts_with("identity.") {
            return Ok(None);
        }
        let input = &invocation.input;
        Ok(Some(match key {
            "identity.enroll" | "identity.login" => {
                if bearer.is_some() {
                    return Err(Error::InvalidInput);
                }
                let object = input
                    .as_object()
                    .filter(|o| o.len() == 2)
                    .ok_or(Error::InvalidInput)?;
                let email = object
                    .get("email")
                    .and_then(Value::as_str)
                    .ok_or(Error::InvalidInput)?;
                let password = object
                    .get("password")
                    .and_then(Value::as_str)
                    .ok_or(Error::InvalidInput)?;
                if key == "identity.enroll" {
                    Self::Enroll {
                        email: email.into(),
                        password: password.into(),
                    }
                } else {
                    Self::Login {
                        email: email.into(),
                        password: password.into(),
                    }
                }
            }
            "identity.current" | "identity.logout" => {
                if !input.is_null() {
                    return Err(Error::InvalidInput);
                }
                let bearer = bearer.ok_or(Error::IdentityRequired)?;
                if key == "identity.current" {
                    Self::Current {
                        bearer: bearer.into(),
                    }
                } else {
                    Self::Logout {
                        bearer: bearer.into(),
                    }
                }
            }
            _ => return Err(Error::UnknownOperation),
        }))
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Enroll { .. } => "identity.enroll",
            Self::Login { .. } => "identity.login",
            Self::Current { .. } => "identity.current",
            Self::Logout { .. } => "identity.logout",
        }
    }
    pub fn execute(
        self,
        identity: &Identity,
        tx: &mut Transaction<'_>,
        crypto: &mut impl Crypto,
        now: i64,
    ) -> Result<Value, snap_store::Error> {
        let issued = match self {
            Self::Enroll { email, password } => {
                identity.enroll(tx, crypto, &email, &password, now)?
            }
            Self::Login { email, password } => {
                identity.login(tx, crypto, &email, &password, now)?
            }
            Self::Current { bearer } => {
                return Ok(json!(identity.resolve(tx, crypto, &bearer, now)?));
            }
            Self::Logout { bearer } => {
                identity.revoke(tx, crypto, &bearer, now)?;
                return Ok(Value::Null);
            }
        };
        Ok(json!({"bearer": issued.bearer, "session": issued.session}))
    }
}
pub fn transport_error(error: snap_store::Error) -> Error {
    match error {
        snap_store::Error::NotFound => Error::InvalidBearer,
        snap_store::Error::Invalid => Error::InvalidInput,
        snap_store::Error::Constraint => Error::Application(json!({"code": "Conflict"})),
        snap_store::Error::Miss(_) => Error::Application(json!({"code": "StoreMiss"})),
        snap_store::Error::Indeterminate => Error::Application(json!({"code": "Indeterminate"})),
        snap_store::Error::Unavailable => Error::Unavailable,
    }
}
