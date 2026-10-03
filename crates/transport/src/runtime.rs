//! Portable execution-loop contract and independent carrier output.
use crate::{Command, Error, Invocation, Response, bearer, carrier::Frame};
use alloc::{collections::VecDeque, sync::Arc};
use spin::Mutex;

/// The native executor drives this contract without knowing which core modules
/// the application selected. Physical socket lifetime remains in the driver.
pub trait Loop {
    /// Allocate a physical observation peer, not an authenticated logical session.
    fn open(&mut self) -> Result<u64, Error>;
    /// Return the same independently drainable output used by execution. Cloning
    /// this handle must not acquire or retain the application's execution gate.
    fn output(&self, peer: u64) -> Result<Output, Error>;
    fn carrier_control(&self, peer: u64) -> Result<CarrierControl, Error>;
    /// Consume carrier teardown before any further protected admission, then
    /// maintain logical lifetimes. Teardown does not cancel accepted work.
    fn tick(&mut self, now: u64);
    /// Execute at most one operation. Publish its terminal frame only after its
    /// commit and any required module reconciliation have finished.
    fn step(&mut self) -> bool;
    /// Queue or admit a command without executing its operation handler. Admission
    /// failures and retained replay may publish immediately; new work waits for step.
    fn submit(&mut self, peer: u64, command: Command, now: u64) -> Result<(), Error>;
    fn retired(&self, peer: u64) -> bool;
    fn authorize_upgrade(&self, bearer: &str) -> Result<(), Error>;
    fn is_preconnection_request(&self, name: &str) -> bool;
    /// Run a private connectionless request through the same FIFO. The native
    /// executor calls this on a blocking worker; issued credentials must not enter
    /// retained retry state or another peer's output.
    fn preconnection_reply(
        &mut self,
        invocation: Invocation,
        bearer: Option<alloc::string::String>,
    ) -> bearer::Reply;
}

/// Socket teardown is recorded without acquiring the execution gate. The loop
/// consumes it before protected admission. Already accepted work still drains.
#[derive(Clone, Default)]
pub struct CarrierControl(Arc<Mutex<Option<(bool, u64)>>>);
impl CarrierControl {
    pub fn detach(&self, now: u64) {
        self.0.lock().get_or_insert((false, now));
    }
    pub fn close(&self, now: u64) {
        *self.0.lock() = Some((true, now));
    }
    pub fn take(&self) -> Option<(bool, u64)> {
        self.0.lock().take()
    }
}

/// Output has its own short-held lock. A carrier can drain progress while the
/// application executor is blocked on a host-owned controller callback.
#[derive(Clone, Default)]
pub struct Output(Arc<Mutex<Outbox>>);
#[derive(Default)]
struct Outbox {
    frames: VecDeque<Frame>,
    sealed: bool,
}
impl Output {
    pub fn pop_front(&self) -> Option<Response> {
        self.pop_frame().map(|frame| frame.response)
    }
    pub fn pop_frame(&self) -> Option<Frame> {
        self.0.lock().frames.pop_front()
    }
    pub fn push_frame(&self, frame: Frame) {
        let mut outbox = self.0.lock();
        if !outbox.sealed {
            outbox.frames.push_back(frame);
        }
    }
    /// Append the final frame and stop physical publication atomically. Queued
    /// frames remain drainable; accepted work can still commit and retain replay
    /// state, but cannot publish into this retired physical output.
    pub fn seal(&self, final_frame: Option<Frame>) {
        let mut outbox = self.0.lock();
        if !outbox.sealed {
            if let Some(frame) = final_frame {
                outbox.frames.push_back(frame);
            }
            outbox.sealed = true;
        }
    }
    pub fn push_back(&self, response: Response) {
        self.push_frame(Frame {
            response,
            handshake: false,
            attachment: None,
            terminal: false,
        });
    }
    pub fn is_empty(&self) -> bool {
        self.0.lock().frames.is_empty()
    }
    pub fn front(&self) -> Option<Response> {
        self.0
            .lock()
            .frames
            .front()
            .map(|frame| frame.response.clone())
    }
    pub fn retain(&self, mut keep: impl FnMut(&Response) -> bool) {
        self.0.lock().frames.retain(|frame| keep(&frame.response));
    }
    /// Reauthorize queued publications after the application's access changes.
    /// The callback must use resident facts only and must not reenter this output.
    pub fn retain_mut(&self, mut keep: impl FnMut(&mut Response) -> bool) {
        self.0
            .lock()
            .frames
            .retain_mut(|frame| keep(&mut frame.response));
    }
}
