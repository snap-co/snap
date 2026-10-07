//! The platform cartridge's SDK driver, not a simulation host. The same world
//! and workload can run on real carriers. Other applications own their own drivers
//! implementing runner::Workload; neither the runner nor host imports this module.
use crate::{
    cartridge::{self, Edit, Stop},
    dispatch::predict,
    journey,
    runner::{Random, Workload},
};
use serde::Serialize;
use snap_transport::{Channel, client::Client, json};

/// Valid cartridge state. Create it through real operations instead of populating
/// a sidecar or bypassing guards. Richer applications define their own world model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct World {
    pub value: i64,
}
impl World {
    pub fn generate(seed: u64) -> Self {
        Self {
            value: Random::stream(seed, "world").below(513) as i64 - 256,
        }
    }
}

/// One action is one SDK invocation. A read follows every attempted change to
/// verify both rows independently. A budget may stop before that follow-up read;
/// each completed change still checks admission and terminal output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Action {
    Read,
    Change(Edit),
}
pub struct Probe<C> {
    client: Client<C>,
    value: i64,
    verify_next: bool,
}
impl<C: Channel> Probe<C> {
    /// Assembly supplies a newly attached client on the cartridge's zero baseline.
    pub fn new(client: Client<C>) -> Self {
        Self {
            client,
            value: 0,
            verify_next: true,
        }
    }
    pub fn expected_value(&self) -> i64 {
        self.value
    }
    pub async fn initialize(&mut self, world: &World) {
        self.execute(Action::Change(Edit {
            expected: 0,
            amount: world.value,
            stop: Stop::Commit,
        }))
        .await;
        self.execute(Action::Read).await;
    }
}
impl<C: Channel> Workload for Probe<C> {
    type Action = Action;
    fn generate(&mut self, random: &mut Random) -> Action {
        if self.verify_next || random.below(8) == 0 {
            return Action::Read;
        }
        let expected = if random.below(4) == 0 {
            self.value
                .checked_add(1)
                .expect("workload compare overflow")
        } else {
            self.value
        };
        Action::Change(Edit {
            expected,
            amount: random.below(17) as i64 - 8,
            stop: [
                Stop::Commit,
                Stop::Commit,
                Stop::Application,
                Stop::InvalidOutput,
                Stop::CaughtMiss,
            ][random.below(5) as usize],
        })
    }
    async fn execute(&mut self, action: Action) {
        let result = match &action {
            Action::Read => {
                self.verify_next = false;
                journey::exchange::<_, cartridge::Read>(
                    &mut self.client,
                    &(),
                    true,
                    Ok(json!([self.value, self.value])),
                    &mut |_| {},
                )
                .await
            }
            Action::Change(edit) => {
                let (accepted, outcome) = predict(&mut self.value, edit);
                self.verify_next = true;
                journey::exchange::<_, cartridge::Change>(
                    &mut self.client,
                    edit,
                    accepted,
                    outcome,
                    &mut |_| {},
                )
                .await
            }
        };
        if let Err(failure) = result {
            panic!(
                "cartridge invariant failed: action={action:?} expected_value={} failure={failure:?}",
                self.value
            );
        }
    }
}
