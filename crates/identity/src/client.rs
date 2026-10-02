//! Portable Rust Identity SDK. Transport owns bearer acquisition and storage;
//! this client sees public authentication facts, never persisted session records.
use crate::{
    CredentialSummary, Principal, ReleaseScope, SessionSummary,
    operation::{self, Proof, ReleaseInput},
};
use alloc::{string::String, vec::Vec};
use snap_transport::{Channel, Error, Operation, client::Client as Transport};

pub struct Client<'a, C> {
    transport: &'a mut Transport<C>,
}
impl<'a, C: Channel> Client<'a, C> {
    pub fn new(transport: &'a mut Transport<C>) -> Self {
        Self { transport }
    }
    pub async fn enroll(&mut self, email: &str, password: &str) -> Result<Principal, Error> {
        self.call::<operation::Enroll>(
            None,
            &Proof {
                email: email.into(),
                password: password.into(),
            },
        )
        .await
    }
    pub async fn acquire(&mut self, email: &str, password: &str) -> Result<Principal, Error> {
        self.call::<operation::Acquire>(
            None,
            &Proof {
                email: email.into(),
                password: password.into(),
            },
        )
        .await
    }
    pub async fn fetch(&mut self) -> Result<Option<Principal>, Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::Fetch>(bearer.as_deref(), &()).await
    }
    pub async fn release(&mut self, scope: ReleaseScope) -> Result<(), Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::Release>(bearer.as_deref(), &ReleaseInput { scope })
            .await
    }
    pub async fn sessions(&mut self) -> Result<Vec<SessionSummary>, Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::ListSessions>(bearer.as_deref(), &())
            .await
    }
    pub async fn credentials(&mut self) -> Result<Vec<CredentialSummary>, Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::ListCredentials>(bearer.as_deref(), &())
            .await
    }
    async fn call<O: Operation>(
        &mut self,
        bearer: Option<&str>,
        input: &O::Input,
    ) -> Result<O::Output, Error> {
        let input = snap_transport::json!(input);
        let value = self.transport.request(bearer, O::NAME, input).await?;
        // All wire decoding and validation lives in the Rust SDK.
        snap_transport::client::decode::<O::Output>(value)
    }
}

/// Stable user-facing acquisition errors shared by native and Wasm bindings.
pub fn message(error: &Error) -> &'static str {
    match error {
        Error::InvalidInput => {
            "Check your email address and password requirements, then try again."
        }
        Error::InvalidBearer | Error::IdentityRequired => {
            "The email or password is incorrect. Check both and try again."
        }
        Error::Application(value) if value["code"] == "Conflict" => {
            "That email is already registered; sign in instead."
        }
        Error::Application(value) if value["code"] == "Forbidden" => {
            "This request couldn't be verified. Reload the page and try again."
        }
        Error::Unavailable => "We couldn't reach the server. Check your connection and try again.",
        _ => "We couldn't complete your sign-in. Please try again shortly.",
    }
}
