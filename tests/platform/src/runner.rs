//! Seeded SDK workload coordination, independent of any application or host.
//! Applications own world generation, legal actions and invariant checks. Hosts
//! own execution, clocks, dependency faults and safety watchdogs.
use alloc::rc::Rc;
use core::{cell::RefCell, future::Future};
use serde::Serialize;
use sha2::{Digest, Sha256};
use snap_transport::{Channel, Command, Error, Response};

/// Deterministic SplitMix64 stream. This is simulation entropy, never a source
/// for production secrets. Named streams do not consume each other's draws.
pub struct Random(u64);
impl Random {
    pub fn stream(seed: u64, name: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"snap-simulation-random-v1");
        hash.update(seed.to_be_bytes());
        hash.update(name.as_bytes());
        let bytes = hash.finalize();
        Self(u64::from_be_bytes(bytes[..8].try_into().unwrap()))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
    /// Uniform selection without modulo bias. An empty choice is a workload bug.
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "random choice must be nonempty");
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return value % bound;
            }
        }
    }
}

/// One action can drive any headless SDK. Its meaning and invariant checks belong
/// to the application, not this runner. Setup/world creation happens before run.
/// An action is counted as completed only after execute returns. Panics propagate.
pub trait Workload {
    type Action: Serialize;
    fn generate(&mut self, random: &mut Random) -> Self::Action;
    fn execute(&mut self, action: Self::Action) -> impl Future<Output = ()>;
}

/// A host can stop between scheduler boundaries without draining pending work.
/// Deadline drops the client future; it does NOT cancel already accepted work.
/// A deadline-stopped workload must not be resumed with an abandoned SDK call.
pub trait Host {
    type Error;
    fn now(&self) -> u64;
    fn drive<F: Future>(
        &mut self,
        future: F,
        deadline_ms: Option<u64>,
    ) -> Result<Drive<F::Output>, Self::Error>;
}
#[derive(Debug, PartialEq, Eq)]
pub enum Drive<T> {
    Complete(T),
    Deadline,
}

#[derive(Clone, Copy, Debug)]
pub enum Budget {
    /// Exactly this many completed workload actions, excluding world setup.
    Operations(u64),
    /// Duration from the campaign's start, excluding world setup. No new action
    /// starts at the horizon. Synchronous callbacks may overrun it; Report says so.
    TimeMs(u64),
}
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub seed: u64,
    pub budget: Budget,
}

/// Replay requires the same code, encodings, world, host config and seeds. These
/// fingerprints compare observed streams; they are not a proof of correctness.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub seed: u64,
    pub started: u64,
    pub completed: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub deadline_ms: Option<u64>,
    pub overrun_ms: u64,
    pub actions_sha256: [u8; 32],
}

pub fn run<H: Host, W: Workload>(
    host: &mut H,
    workload: &mut W,
    config: Config,
) -> Result<Report, H::Error> {
    let start_ms = host.now();
    let deadline_ms = match config.budget {
        Budget::Operations(_) => None,
        Budget::TimeMs(duration) => Some(
            start_ms
                .checked_add(duration)
                .expect("campaign deadline overflow"),
        ),
    };
    let mut random = Random::stream(config.seed, "workload");
    let mut actions = Fingerprint::new(b"snap-workload-actions-v1");
    let mut started = 0;
    let mut completed = 0;
    loop {
        if match config.budget {
            Budget::Operations(count) => completed == count,
            Budget::TimeMs(_) => host.now() >= deadline_ms.unwrap(),
        } {
            break;
        }
        let action = workload.generate(&mut random);
        actions.record(&action);
        started += 1;
        match host.drive(workload.execute(action), deadline_ms)? {
            Drive::Complete(()) => completed += 1,
            Drive::Deadline => break,
        }
    }
    let end_ms = host.now();
    Ok(Report {
        seed: config.seed,
        started,
        completed,
        start_ms,
        end_ms,
        deadline_ms,
        overrun_ms: deadline_ms.map_or(0, |deadline| end_ms.saturating_sub(deadline)),
        actions_sha256: actions.finish(),
    })
}

/// Length-framed, key-sorted JSON, not DefaultHasher or Debug output. Domains and
/// encoding are versioned. Changing these invalidates old fingerprint comparisons.
#[derive(Clone)]
pub(crate) struct Fingerprint(Sha256);
impl Fingerprint {
    pub(crate) fn new(domain: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(domain);
        Self(hash)
    }
    pub(crate) fn record(&mut self, value: &impl Serialize) {
        let mut value = serde_json::to_value(value).expect("replay record must serialize");
        value.sort_all_objects();
        let bytes = serde_json::to_vec(&value).expect("replay record must encode");
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }
    pub(crate) fn finish(&self) -> [u8; 32] {
        self.0.clone().finalize().into()
    }
}

/// Streaming recording at the actual SDK Channel seam. Includes physical Connect
/// commands and world setup when wrapped before setup. IDs and payloads are part
/// of the fingerprint; timing is not. No entire history is retained in memory.
/// Send records only successful submissions; receive records observations/errors.
/// A successful send promises carrier handoff, not acceptance or commit.
#[derive(Clone)]
pub struct Transcript(Rc<RefCell<Streams>>);
struct Streams {
    commands: Fingerprint,
    observations: Fingerprint,
    sent: u64,
    received: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub struct TranscriptSummary {
    pub sent: u64,
    pub received: u64,
    pub commands_sha256: [u8; 32],
    pub observations_sha256: [u8; 32],
}
impl Default for Transcript {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(Streams {
            commands: Fingerprint::new(b"snap-sdk-commands-v1"),
            observations: Fingerprint::new(b"snap-sdk-observations-v1"),
            sent: 0,
            received: 0,
        })))
    }
}
impl Transcript {
    pub fn channel<C: Channel>(&self, channel: C) -> Recording<C> {
        Recording {
            channel,
            transcript: self.clone(),
        }
    }
    pub fn summary(&self) -> TranscriptSummary {
        let streams = self.0.borrow();
        TranscriptSummary {
            sent: streams.sent,
            received: streams.received,
            commands_sha256: streams.commands.finish(),
            observations_sha256: streams.observations.finish(),
        }
    }
}
pub struct Recording<C> {
    channel: C,
    transcript: Transcript,
}
impl<C: Channel> Channel for Recording<C> {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        let record = command.clone();
        self.channel.send(command).await?;
        let mut streams = self.transcript.0.borrow_mut();
        streams.commands.record(&record);
        streams.sent += 1;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        let result = self.channel.receive().await;
        let mut streams = self.transcript.0.borrow_mut();
        streams.observations.record(&result);
        streams.received += 1;
        result
    }
}

pub fn hex(digest: &[u8; 32]) -> alloc::string::String {
    use core::fmt::Write;
    let mut output = alloc::string::String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}
