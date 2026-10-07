//! Portable Rust Identity SDK. Transport owns bearer acquisition and storage;
//! this client sees public authentication facts, never persisted session records.
use crate::{
    CredentialSummary, Principal, ReleaseScope, SessionSummary,
    operation::{self, Proof, ReleaseInput},
};
use alloc::{string::String, vec::Vec};
use snap_transport::{Error, Operation, client::Operations};

pub struct Client<'a, C> {
    transport: &'a mut C,
}
impl<'a, C: Operations> Client<'a, C> {
    pub fn new(transport: &'a mut C) -> Self {
        Self { transport }
    }
    pub async fn enroll(&mut self, email: &str, password: &str) -> Result<Principal, Error> {
        self.call::<operation::Enroll>(&Proof {
            email: email.into(),
            password: password.into(),
        })
        .await
    }
    pub async fn acquire(&mut self, email: &str, password: &str) -> Result<Principal, Error> {
        self.call::<operation::Acquire>(&Proof {
            email: email.into(),
            password: password.into(),
        })
        .await
    }
    pub async fn fetch(&mut self) -> Result<Option<Principal>, Error> {
        self.call::<operation::Fetch>(&()).await
    }
    pub async fn release(&mut self, scope: ReleaseScope) -> Result<(), Error> {
        self.call::<operation::Release>(&ReleaseInput { scope })
            .await
    }
    pub async fn sessions(&mut self) -> Result<Vec<SessionSummary>, Error> {
        self.call::<operation::ListSessions>(&()).await
    }
    pub async fn credentials(&mut self) -> Result<Vec<CredentialSummary>, Error> {
        self.call::<operation::ListCredentials>(&()).await
    }
    pub async fn remove_credential(&mut self, locator: &str) -> Result<(), Error> {
        self.call::<operation::RemoveCredential>(&operation::CredentialInput {
            locator: locator.into(),
        })
        .await
    }
    pub async fn link_password(&mut self, email: &str, password: &str) -> Result<(), Error> {
        self.call::<operation::LinkPassword>(&Proof {
            email: email.into(),
            password: password.into(),
        })
        .await
    }
    pub async fn rename_credential(&mut self, locator: &str, label: &str) -> Result<(), Error> {
        self.call::<operation::RenameCredential>(&operation::RenameInput {
            locator: locator.into(),
            label: label.into(),
        })
        .await
    }
    pub async fn begin_passkey_registration(
        &mut self,
        label: &str,
        binding: &str,
    ) -> Result<crate::passkey::Challenge, Error> {
        self.call::<operation::BeginRegistration>(&operation::RegistrationInput {
            label: label.into(),
            binding: binding.into(),
        })
        .await
    }
    pub async fn finish_passkey_registration(
        &mut self,
        proof: &operation::PasskeyProof,
    ) -> Result<Principal, Error> {
        self.call::<operation::FinishRegistration>(proof).await
    }
    pub async fn begin_passkey_authentication(
        &mut self,
        locator: Option<&str>,
        binding: &str,
    ) -> Result<crate::passkey::Challenge, Error> {
        self.call::<operation::BeginAuthentication>(&operation::AuthenticationInput {
            locator: locator.map(String::from),
            name: None,
            binding: binding.into(),
        })
        .await
    }
    pub async fn begin_named_passkey_authentication(
        &mut self,
        name: &str,
        binding: &str,
    ) -> Result<crate::passkey::Challenge, Error> {
        self.call::<operation::BeginAuthentication>(&operation::AuthenticationInput {
            locator: None,
            name: Some(name.into()),
            binding: binding.into(),
        })
        .await
    }
    pub async fn finish_passkey_authentication(
        &mut self,
        proof: &operation::PasskeyProof,
    ) -> Result<Principal, Error> {
        self.call::<operation::FinishAuthentication>(proof).await
    }
    async fn call<O: Operation>(&mut self, input: &O::Input) -> Result<O::Output, Error> {
        self.transport.call::<O>(input).await
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
