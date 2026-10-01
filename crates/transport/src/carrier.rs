//! Domain-independent handoff between physical carriers and host dispatch.
//! Hosts select synchronization and execution; this contract requires neither
//! threads nor an async runtime. Drivers never run handlers or access Store.
use crate::{Command, Error, Invocation, Outcome, Response};
use alloc::string::String;
use core::future::Future;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub retention_ms: u64,
    /// Opaque boot/lifetime namespace. A resumed flag alone cannot establish
    /// continuity after a lost handshake reply. Only expose after authentication.
    pub lifetime: String,
}

/// Host observation, independent of wire framing. `handshake` identifies a reply
/// to Connect; `terminal` closes a one-shot exchange after writing this reply.
#[derive(Clone, Debug)]
pub struct Frame {
    pub response: Response,
    pub handshake: bool,
    pub attachment: Option<AttachmentInfo>,
    pub terminal: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Submission {
    Queued,
    /// Host policy requests physical teardown, without waiting for execution.
    CloseSocket,
}

pub trait Connection: Send + Sync + 'static {
    /// Queue only. Success is a handoff, not operation acceptance or completion.
    /// Must not execute application code or acquire its execution gate. `bytes`
    /// charges the decoded command's wire size against the pending byte budget.
    fn submit(&self, command: Command, bytes: usize) -> Result<Submission, Error>;
    /// Drain one already-published observation without the execution gate.
    /// The host owns authorization and ordering before publishing observations.
    fn receive(&self) -> Option<Frame>;
    fn retired(&self) -> bool;
    /// Physical loss preserves logical residency. Teardown must remain available
    /// while application execution is busy and cannot downgrade a logical Close.
    fn disconnect(&self);
}

pub trait Dispatch: Clone + Send + Sync + 'static {
    type Connection: Connection;
    /// Host-side opening and optional credential validation. Drivers supply a
    /// credential only when their configured upgrade policy requires validation.
    fn open(
        &self,
        credential: Option<String>,
        max_pending_bytes: usize,
    ) -> impl Future<Output = Result<Self::Connection, Error>> + Send;
    /// One connectionless HTTP exchange. Execution belongs to host dispatch;
    /// the carrier may await its result, but must not execute it or replay it.
    fn request(
        &self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> impl Future<Output = Outcome> + Send;
}

/// Own physical teardown across EOF, errors, failed upgrades and task cancellation.
/// Disconnect must not downgrade a previously requested logical Close.
pub struct Physical<C: Connection>(pub C);
impl<C: Connection> Drop for Physical<C> {
    fn drop(&mut self) {
        self.0.disconnect();
    }
}
