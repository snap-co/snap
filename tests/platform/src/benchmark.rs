//! Portable closed-loop measurement. Workloads own SDK calls and validation;
//! adapters own execution and clocks. No setup, warmup or final checks occur here.
use crate::runner::join;
use alloc::{boxed::Box, string::String, vec::Vec};
use core::{future::poll_fn, task::Poll};
use serde::{Deserialize, Serialize};

/// Monotonic nanoseconds in the adapter's declared clock domain. A virtual clock
/// measures modeled latency, never the CPU time of a synchronous host callback.
pub trait Clock {
    fn now_ns(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Read,
    Commit,
    Conflict,
}

/// Each call must execute exactly one SDK invocation, without retry or think time.
/// Unexpected failures are errors, not throughput. Other apps implement this
/// contract with their own precomputed inputs and lightweight outcome checks.
pub trait Workload {
    fn execute(&mut self) -> impl core::future::Future<Output = Result<Outcome, String>>;
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub reads: u64,
    pub commits: u64,
    pub conflicts: u64,
}
impl Counts {
    pub fn completed(&self) -> u64 {
        self.reads + self.commits + self.conflicts
    }
    pub fn successful(&self) -> u64 {
        self.reads + self.commits
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Latency {
    pub samples: u64,
    /// Exact nearest-rank percentiles; absent when this outcome did not occur.
    pub p50_ns: Option<u64>,
    pub p95_ns: Option<u64>,
    pub p99_ns: Option<u64>,
}
impl Latency {
    fn summarize(mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        let percentile = |percent: usize| {
            (!values.is_empty()).then(|| values[(values.len() * percent).div_ceil(100) - 1])
        };
        Self {
            samples: values.len() as u64,
            p50_ns: percentile(50),
            p95_ns: percentile(95),
            p99_ns: percentile(99),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Report {
    pub counts: Counts,
    pub actors: Vec<Counts>,
    pub wall_elapsed_ns: u64,
    pub response_elapsed_ns: u64,
    pub read_latency: Latency,
    pub commit_latency: Latency,
    pub conflict_latency: Latency,
}

/// Fixed input ownership, independent of completion order. Actors run without
/// round barriers but drain their assigned counts; the tail can underfill load.
/// Changing concurrency changes the input partition and must use a new baseline.
pub fn actor_operations(total: u64, actors: usize, actor: usize) -> u64 {
    assert!(actors > 0 && actor < actors);
    total / actors as u64 + u64::from((actor as u64) < total % actors as u64)
}

/// Clocks and the executor are adapter supplied. Timing includes SDK encoding,
/// lightweight validation and sampling; percentile sorting happens after timing.
/// Exact latency retention costs O(operations) memory, not constant memory.
pub async fn run<W: Workload>(
    actors: &mut [W],
    operations: u64,
    response_clock: &impl Clock,
    wall_clock: &impl Clock,
) -> Result<Report, String> {
    if actors.is_empty() {
        return Err("benchmark needs at least one actor".into());
    }
    let actor_count = actors.len();
    let mut results: Vec<Result<Vec<(Outcome, u64)>, String>> =
        (0..actor_count).map(|_| Ok(Vec::new())).collect();
    // Reserve before timing, so allocation size does not distort the start.
    for (index, result) in results.iter_mut().enumerate() {
        let count = usize::try_from(actor_operations(operations, actor_count, index))
            .map_err(|_| String::from("operation count exceeds address space"))?;
        result
            .as_mut()
            .unwrap()
            .try_reserve_exact(count)
            .map_err(|_| String::from("cannot allocate latency samples"))?;
    }
    let tasks = actors
        .iter_mut()
        .zip(&mut results)
        .enumerate()
        .map(|(index, (actor, result))| {
            Box::pin(async move {
                for _ in 0..actor_operations(operations, actor_count, index) {
                    let start = response_clock.now_ns();
                    let outcome = match actor.execute().await {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            *result = Err(error);
                            return;
                        }
                    };
                    let Some(elapsed) = response_clock.now_ns().checked_sub(start) else {
                        *result = Err("response clock moved backwards".into());
                        return;
                    };
                    result.as_mut().unwrap().push((outcome, elapsed));
                    // Even an immediately ready actor must let peers and IO progress.
                    let mut yielded = false;
                    poll_fn(|cx| {
                        if core::mem::replace(&mut yielded, true) {
                            Poll::Ready(())
                        } else {
                            cx.waker().wake_by_ref();
                            Poll::Pending
                        }
                    })
                    .await;
                }
            }) as core::pin::Pin<Box<dyn core::future::Future<Output = ()> + '_>>
        })
        .collect();
    let wall_start = wall_clock.now_ns();
    let response_start = response_clock.now_ns();
    join(tasks).await;
    let wall_elapsed_ns = wall_clock
        .now_ns()
        .checked_sub(wall_start)
        .ok_or_else(|| String::from("wall clock moved backwards"))?;
    let response_elapsed_ns = response_clock
        .now_ns()
        .checked_sub(response_start)
        .ok_or_else(|| String::from("response clock moved backwards"))?;
    let mut reads = Vec::new();
    let mut commits = Vec::new();
    let mut conflicts = Vec::new();
    let mut counts = Counts::default();
    let mut actors = Vec::new();
    for result in results {
        let mut local = Counts::default();
        for (outcome, latency) in result? {
            match outcome {
                Outcome::Read => {
                    local.reads += 1;
                    reads.push(latency);
                }
                Outcome::Commit => {
                    local.commits += 1;
                    commits.push(latency);
                }
                Outcome::Conflict => {
                    local.conflicts += 1;
                    conflicts.push(latency);
                }
            }
        }
        counts.reads += local.reads;
        counts.commits += local.commits;
        counts.conflicts += local.conflicts;
        actors.push(local);
    }
    Ok(Report {
        counts,
        actors,
        wall_elapsed_ns,
        response_elapsed_ns,
        read_latency: Latency::summarize(reads),
        commit_latency: Latency::summarize(commits),
        conflict_latency: Latency::summarize(conflicts),
    })
}
