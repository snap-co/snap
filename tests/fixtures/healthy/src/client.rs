//! Healthy's client application. Polling policy and all monitor state live here.
use alloc::vec::Vec;
use serde::Serialize;
use snap_client::{
    Client, HealthQuery,
    application::{Application, Input, Step},
};

pub const POLL_MS: u32 = 2_000;
const MAX_SAMPLES: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Loading,
    Ok,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub ok: bool,
    pub at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub status: Status,
    pub samples: Vec<Sample>,
}

pub struct Healthy {
    client: Client,
    pending: Option<HealthQuery>,
    snapshot: Snapshot,
}

pub fn application() -> Healthy {
    Healthy {
        client: Client::default(),
        pending: None,
        snapshot: Snapshot {
            status: Status::Loading,
            samples: Vec::new(),
        },
    }
}

impl Application for Healthy {
    type Snapshot = Snapshot;

    fn update(&mut self, input: Input) -> Step {
        match input {
            Input::Start | Input::Wake => match self.client.health_up() {
                Ok(query) => {
                    let invocation = query.invocation().clone();
                    self.pending = Some(query);
                    Step::Query(invocation)
                }
                Err(_) => {
                    self.snapshot.status = Status::Error;
                    Step::Stop
                }
            },
            Input::Completed { result, at } => {
                let Some(query) = self.pending.take() else {
                    return Step::Stop;
                };
                let ok = result.and_then(|wire| query.complete(&wire)).is_ok();
                self.snapshot.status = if ok { Status::Ok } else { Status::Error };
                if self.snapshot.samples.len() == MAX_SAMPLES {
                    self.snapshot.samples.remove(0);
                }
                self.snapshot.samples.push(Sample { ok, at });
                Step::Wait {
                    milliseconds: POLL_MS,
                }
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
}
