//! The portable per-connection handoff.
//!
//! A carrier decodes a command and queues it here; the application takes commands
//! out on its own thread and publishes finished replies back in. Neither side
//! executes the other's code, and neither side holds the other's state.
//!
//! The queue itself is [`snap_store::inbox::Channel`], a platform-agnostic
//! single-producer ring in the Store interface. This type adds only what Transport
//! needs on top of it: the carrier's [`Connection`] contract, and the two lifecycle
//! flags that separate a dead socket from a closed session.
//!
//! It lives here, in the interface tier, because it is what makes a driver a
//! driver. A WebSocket and a TLS socket perform different IO and then feed an
//! identical inbox; nothing in this file knows which.
use crate::{
    Command, Error, Response,
    carrier::{Connection, Frame, Submission},
};
use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};
use snap_store::inbox::{Channel, Rejected};

/// One client's inbox. The carrier fills `requests` with decoded commands; the
/// application empties them and fills `replies` with finished observations.
#[derive(Debug)]
pub struct Inbox {
    requests: Channel<Command>,
    replies: Channel<Frame>,
    /// Physical loss. Preserves logical residency, matching `disconnect`.
    detached: AtomicBool,
    /// Logical close. Set by the application once it has published its final
    /// frames; the carrier must drain them before tearing the socket down.
    retired: AtomicBool,
}

impl Inbox {
    /// Upper bound on queued items per direction. Depth and byte budget are
    /// independent limits: a carrier passes a large budget (`serve` offers 16 MiB)
    /// but a connection is made of a few commands at a time, and slots are
    /// allocated eagerly. Deriving depth from the budget would reserve hundreds of
    /// megabytes per connection to hold messages that never arrive.
    const MAX_DEPTH: usize = 1024;

    /// `budget` is the byte allowance a carrier receives from its platform's
    /// `open` call. Both directions draw on it independently.
    pub fn new(budget: usize) -> Self {
        // Assume small messages for the depth estimate; the byte budget is what
        // actually bounds a large message, and `Channel` refuses one either way.
        let depth = (budget / 1024).clamp(16, Self::MAX_DEPTH);
        Self {
            requests: Channel::new(depth, budget),
            replies: Channel::new(depth, budget),
            detached: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        }
    }

    /// Queued item capacity per direction, after rounding to a power of two.
    pub fn depth(&self) -> usize {
        self.requests.capacity()
    }

    // ---- application side -------------------------------------------------

    /// Take the next queued command, if the carrier published one. Draining
    /// releases the byte reservation that command carried.
    pub fn next_command(&self) -> Option<Command> {
        self.requests.pop().map(|(command, _)| command)
    }

    /// Publish an observation for the carrier to write to its socket. `bytes` is its
    /// wire size.
    ///
    /// Returns `false` when the reply direction is full. See [`Self::publish_frame`].
    pub fn publish(
        &self,
        response: Response,
        handshake: bool,
        terminal: bool,
        bytes: usize,
    ) -> bool {
        self.publish_frame(
            Frame {
                response,
                handshake,
                attachment: None,
                terminal,
            },
            bytes,
        )
    }

    /// Publish the handshake reply, carrying the attachment a resumed logical
    /// connection must present on later commands.
    pub fn publish_attachment(
        &self,
        response: Response,
        retention_ms: u64,
        lifetime: String,
        bytes: usize,
    ) -> bool {
        self.publish_frame(
            Frame {
                response,
                handshake: true,
                attachment: Some(crate::carrier::AttachmentInfo {
                    retention_ms,
                    lifetime,
                }),
                terminal: false,
            },
            bytes,
        )
    }

    /// Publish a pre-framed observation. `bytes` is its wire size.
    ///
    /// Returns `false` when the reply direction is full. The application is the
    /// producer here, so it can apply backpressure and decide whether to hold the
    /// frame or drop it; losing one silently would strand a peer waiting on a
    /// reply it will never receive.
    pub fn publish_frame(&self, frame: Frame, bytes: usize) -> bool {
        self.replies.push(frame, bytes).is_ok()
    }

    /// Mark the logical connection closed. The carrier must still drain
    /// `receive` until it observes `retired`, then tear the socket down.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }

    pub fn queued(&self) -> usize {
        self.requests.len()
    }

    pub fn pending(&self) -> usize {
        self.replies.len()
    }
}

impl Connection for Inbox {
    /// Carrier side. Publish only; never execute, never acquire the
    /// application's gate.
    fn submit(&self, command: Command, bytes: usize) -> Result<Submission, Error> {
        if self.retired() {
            return Ok(Submission::CloseSocket);
        }
        match self.requests.push(command, bytes) {
            Ok(()) => Ok(Submission::Queued),
            // The refused command comes back; dropping it here is the carrier's
            // decision to backpressure rather than the queue's.
            Err((_, Rejected::Full)) => Err(Error::Capacity),
        }
    }

    /// Carrier side. Drain an observation the application already published.
    fn receive(&self) -> Option<Frame> {
        self.replies.pop().map(|(frame, _)| frame)
    }

    /// Carrier side. Retirement is the application's decision and is observed
    /// after its final frames have been written.
    fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// Carrier side. Physical loss only; logical residency survives it.
    fn disconnect(&self) {
        self.detached.store(true, Ordering::Release);
    }
}
