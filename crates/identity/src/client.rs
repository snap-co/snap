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
    pub async fn remove_credential(&mut self, locator: &str) -> Result<(), Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::RemoveCredential>(
            bearer.as_deref(),
            &operation::CredentialInput {
                locator: locator.into(),
            },
        )
        .await
    }
    pub async fn link_password(&mut self, email: &str, password: &str) -> Result<(), Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::LinkPassword>(
            bearer.as_deref(),
            &Proof {
                email: email.into(),
                password: password.into(),
            },
        )
        .await
    }
    pub async fn rename_credential(&mut self, locator: &str, label: &str) -> Result<(), Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::RenameCredential>(
            bearer.as_deref(),
            &operation::RenameInput {
                locator: locator.into(),
                label: label.into(),
            },
        )
        .await
    }
    pub async fn begin_passkey_registration(
        &mut self,
        label: &str,
        binding: &str,
    ) -> Result<crate::passkey::Challenge, Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::BeginRegistration>(
            bearer.as_deref(),
            &operation::RegistrationInput {
                label: label.into(),
                binding: binding.into(),
            },
        )
        .await
    }
    pub async fn finish_passkey_registration(
        &mut self,
        proof: &operation::PasskeyProof,
    ) -> Result<Principal, Error> {
        let bearer = self.transport.bearer().map(String::from);
        self.call::<operation::FinishRegistration>(bearer.as_deref(), proof)
            .await
    }
    pub async fn begin_passkey_authentication(
        &mut self,
        locator: Option<&str>,
        binding: &str,
    ) -> Result<crate::passkey::Challenge, Error> {
        self.call::<operation::BeginAuthentication>(
            None,
            &operation::AuthenticationInput {
                locator: locator.map(String::from),
                name: None,
                binding: binding.into(),
            },
        )
        .await
    }
    pub async fn begin_named_passkey_authentication(
        &mut self,
        name: &str,
        binding: &str,
    ) -> Result<crate::passkey::Challenge, Error> {
        self.call::<operation::BeginAuthentication>(
            None,
            &operation::AuthenticationInput {
                locator: None,
                name: Some(name.into()),
                binding: binding.into(),
            },
        )
        .await
    }
    pub async fn finish_passkey_authentication(
        &mut self,
        proof: &operation::PasskeyProof,
    ) -> Result<Principal, Error> {
        self.call::<operation::FinishAuthentication>(None, proof)
            .await
    }
    async fn call<O: Operation>(
        &mut self,
        bearer: Option<&str>,
        input: &O::Input,
    ) -> Result<O::Output, Error> {
        let input = snap_transport::json!(input);
        // Sends, then pumps the stream until this invocation completes. All wire
        // decoding and validation lives in the Rust SDK.
        let value = self.transport.call(bearer, O::NAME, input).await?;
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
