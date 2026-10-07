//! Seeded SDK workload coordination, independent of any application or host.
//! Applications own world generation, legal actions and invariant checks. Hosts
//! own execution, clocks, dependency faults and safety watchdogs.
use alloc::{boxed::Box, rc::Rc, sync::Arc, task::Wake, vec::Vec};
use core::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};
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
    /// Application-owned validation after a fully completed concurrent round.
    /// Not called for partial rounds abandoned at a time horizon.
    fn check_round(_actors: &mut [Self])
    where
        Self: Sized,
    {
    }
}

/// Platform-owned timer. Application workloads need no simulation imports.
pub trait Timer: Clone {
    fn sleep(&self, milliseconds: u64) -> impl Future<Output = ()>;
}

struct TaskWake {
    ready: AtomicBool,
    parent: Waker,
}
impl Wake for TaskWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, Ordering::SeqCst);
        self.parent.wake_by_ref();
    }
}

/// Poll each child only after its own wake. Children may borrow application SDKs;
/// no thread, runtime dependency or unconditional polling hides missing wakeups.
/// Dropping this future drops all children, including any pending timer handles.
pub async fn join<'a>(tasks: Vec<Pin<Box<dyn Future<Output = ()> + 'a>>>) {
    let mut tasks: Vec<_> = tasks
        .into_iter()
        .map(|task| (Some(task), None::<Arc<TaskWake>>))
        .collect();
    poll_fn(|context| {
        let mut pending = false;
        for (task, wake) in &mut tasks {
            let Some(future) = task else {
                continue;
            };
            if wake
                .as_ref()
                .is_none_or(|wake| !wake.parent.will_wake(context.waker()))
            {
                *wake = Some(Arc::new(TaskWake {
                    ready: AtomicBool::new(true),
                    parent: context.waker().clone(),
                }));
            }
            let wake = wake.as_ref().unwrap();
            if wake.ready.swap(false, Ordering::SeqCst) {
                let waker = Waker::from(wake.clone());
                if future
                    .as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_ready()
                {
                    *task = None;
                    continue;
                }
            }
            pending = true;
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActorReport {
    pub started: u64,
    pub completed: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub struct ConcurrentReport {
    pub campaign: Report,
    pub actors: Vec<ActorReport>,
    pub rounds: u64,
    pub max_in_flight: u64,
    pub completion_sha256: [u8; 32],
}

/// Bounded concurrent rounds, one action per actor per round. All selected actors
/// can wait on SDK IO simultaneously; the host remains free to serialize work.
/// Operation budgets count actions across ALL actors. Reserve no more than the
/// remaining budget, so the final round may contain fewer actors. Selection rotates
/// fairly and poll order is seeded. Each actor has its own generation stream.
/// Completion order and actor identity are fingerprinted. Checkpoints between
/// rounds permit independent application checks with O(actor count) memory.
pub fn run_many<H: Host, W: Workload>(
    host: &mut H,
    actors: &mut [W],
    config: Config,
) -> Result<ConcurrentReport, H::Error> {
    assert!(!actors.is_empty(), "campaign needs at least one actor");
    let start_ms = host.now();
    let deadline_ms = match config.budget {
        Budget::Operations(_) => None,
        Budget::TimeMs(duration) => Some(
            start_ms
                .checked_add(duration)
                .expect("campaign deadline overflow"),
        ),
    };
    let mut random: Vec<_> = (0..actors.len())
        .map(|actor| Random::stream(config.seed, &alloc::format!("workload/{actor}")))
        .collect();
    let mut order = Random::stream(config.seed, "actor-order");
    let mut actions = Fingerprint::new(b"snap-concurrent-actions-v1");
    let completions = RefCell::new(Fingerprint::new(b"snap-concurrent-completions-v1"));
    let reports = RefCell::new(alloc::vec![ActorReport::default(); actors.len()]);
    let mut started = 0;
    let mut completed = 0;
    let mut rounds = 0;
    let mut cursor = 0;
    let mut max_in_flight = 0;
    loop {
        if match config.budget {
            Budget::Operations(count) => completed == count,
            Budget::TimeMs(_) => host.now() >= deadline_ms.unwrap(),
        } {
            break;
        }
        let count = match config.budget {
            Budget::Operations(limit) => (limit - completed).min(actors.len() as u64) as usize,
            Budget::TimeMs(_) => actors.len(),
        };
        max_in_flight = max_in_flight.max(count as u64);
        let mut selected = alloc::vec![false; actors.len()];
        for offset in 0..count {
            selected[(cursor + offset) % actors.len()] = true;
        }
        cursor = (cursor + count) % actors.len();
        let mut tasks = Vec::new();
        for (actor, workload) in actors.iter_mut().enumerate() {
            if !selected[actor] {
                continue;
            }
            let action = workload.generate(&mut random[actor]);
            actions.record(&(actor, &action));
            reports.borrow_mut()[actor].started += 1;
            started += 1;
            let reports = &reports;
            let completions = &completions;
            tasks.push(Box::pin(async move {
                workload.execute(action).await;
                reports.borrow_mut()[actor].completed += 1;
                completions
                    .borrow_mut()
                    .record(&(actor, reports.borrow()[actor].completed));
            }) as Pin<Box<dyn Future<Output = ()>>>);
        }
        for index in (1..tasks.len()).rev() {
            let other = order.below((index + 1) as u64) as usize;
            tasks.swap(index, other);
        }
        let result = host.drive(join(tasks), deadline_ms)?;
        completed = reports.borrow().iter().map(|report| report.completed).sum();
        if matches!(result, Drive::Deadline) {
            break;
        }
        W::check_round(actors);
        rounds += 1;
    }
    let end_ms = host.now();
    Ok(ConcurrentReport {
        campaign: Report {
            seed: config.seed,
            started,
            completed,
            start_ms,
            end_ms,
            deadline_ms,
            overrun_ms: deadline_ms.map_or(0, |deadline| end_ms.saturating_sub(deadline)),
            actions_sha256: actions.finish(),
        },
        actors: reports.into_inner(),
        rounds,
        max_in_flight,
        completion_sha256: completions.into_inner().finish(),
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
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct TranscriptSummary {
    pub sent: u64,
    pub received: u64,
    pub commands_sha256: [u8; 32],
    pub observations_sha256: [u8; 32],
}
impl TranscriptSummary {
    /// Stable actor-indexed combination. Actor identity prevents equal local IDs
    /// and identical payloads on different clients from collapsing into one stream.
    /// Global interleaving is recorded separately by timed host events/completions.
    pub fn combine(actors: &[Self]) -> Self {
        let mut commands = Fingerprint::new(b"snap-sdk-actor-commands-v1");
        let mut observations = Fingerprint::new(b"snap-sdk-actor-observations-v1");
        for (actor, summary) in actors.iter().enumerate() {
            commands.record(&(actor, summary.sent, summary.commands_sha256));
            observations.record(&(actor, summary.received, summary.observations_sha256));
        }
        Self {
            sent: actors.iter().map(|actor| actor.sent).sum(),
            received: actors.iter().map(|actor| actor.received).sum(),
            commands_sha256: commands.finish(),
            observations_sha256: observations.finish(),
        }
    }
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
