//! Bounded contention oracle for the two-row cartridge. Each mutation round uses
//! one baseline compare and positive, actor-distinct increments. Consequently at
//! most one commit can win. Observations select that possible ordering, but never
//! supply expected row values: the model computes the winner's value from INPUTS.
//! Verification rounds reload both rows through every SDK. This does not model
//! arbitrary multi-operation linearizability or unknown outcomes after carrier loss.
use super::{Action, World};
use crate::{
    cartridge::{Change, Edit, Read, Stop},
    dispatch::predict,
    runner::{Random, Timer, Workload},
};
use alloc::{rc::Rc, vec::Vec};
use core::cell::{Cell, RefCell};
use serde::Serialize;
use snap_transport::{
    Channel, Error, Operation, Outcome,
    client::{Client, Pump},
    json,
};

/// Credits come from external fault INPUTS, not server outcomes. Each observed
/// confirmed rejection consumes one credit, preventing unexplained Store failures
/// from being accepted as random faults. Coalesced fault injections may leave credits.
#[derive(Clone, Default)]
pub struct Rejections(Rc<Cell<u64>>);
impl Rejections {
    pub fn arm(&self) {
        self.0
            .set(self.0.get().checked_add(1).expect("fault credit overflow"));
    }
    fn consume(&self) {
        self.0.set(
            self.0
                .get()
                .checked_sub(1)
                .expect("commit rejection without an injected fault"),
        );
    }
}

struct Model {
    value: i64,
    verify: bool,
    rejections: Rejections,
    sequence: u64,
}
struct Attempt {
    action: Action,
    accepted: bool,
    outcome: Outcome,
    issued: u64,
    finished: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Input {
    pub delay_ms: u64,
    pub action: Action,
}
pub struct Probe<C, T> {
    client: Client<C>,
    timer: T,
    model: Rc<RefCell<Model>>,
    actor: usize,
    actors: usize,
    result: Option<Attempt>,
}
impl<C: Channel, T: Timer> Probe<C, T> {
    /// Clients must already be attached and the world initialized through the SDK.
    pub fn actors(
        clients: Vec<Client<C>>,
        timer: T,
        world: &World,
        rejections: Rejections,
    ) -> Vec<Self> {
        assert!(!clients.is_empty(), "workload needs clients");
        let actors = clients.len();
        let model = Rc::new(RefCell::new(Model {
            value: world.value,
            verify: false,
            rejections,
            sequence: 0,
        }));
        clients
            .into_iter()
            .enumerate()
            .map(|(actor, client)| Self {
                client,
                timer: timer.clone(),
                model: model.clone(),
                actor,
                actors,
                result: None,
            })
            .collect()
    }
    pub fn expected_value(&self) -> i64 {
        self.model.borrow().value
    }
    fn stamp(&self) -> u64 {
        let mut model = self.model.borrow_mut();
        model.sequence = model
            .sequence
            .checked_add(1)
            .expect("observation sequence overflow");
        model.sequence
    }
}
impl<C: Channel, T: Timer> Workload for Probe<C, T> {
    type Action = Input;
    fn generate(&mut self, random: &mut Random) -> Input {
        assert!(self.result.is_none(), "previous round must be checked");
        let model = self.model.borrow();
        let action = if model.verify {
            Action::Read
        } else {
            Action::Change(Edit {
                expected: if random.below(8) == 0 {
                    model.value - 1
                } else {
                    model.value
                },
                amount: (self.actor + 1) as i64 + random.below(8) as i64 * self.actors as i64,
                stop: [
                    Stop::Commit,
                    Stop::Commit,
                    Stop::Commit,
                    Stop::Application,
                    Stop::InvalidOutput,
                    Stop::CaughtMiss,
                ][random.below(6) as usize],
            })
        };
        Input {
            delay_ms: random.below(31),
            action,
        }
    }
    async fn execute(&mut self, input: Input) {
        self.timer.sleep(input.delay_ms).await;
        let issued = self.stamp();
        let (operation, value) = match &input.action {
            Action::Read => (Read::NAME, json!(null)),
            Action::Change(edit) => (Change::NAME, serde_json::to_value(edit).unwrap()),
        };
        let id = self
            .client
            .begin(operation, value)
            .await
            .expect("cartridge send failed");
        let first = self.client.pump().await.expect("cartridge receive failed");
        let (accepted, outcome) = match first {
            Pump::Accepted { id: actual } if actual == id => {
                match self
                    .client
                    .pump()
                    .await
                    .expect("cartridge completion failed")
                {
                    Pump::Completed {
                        id: actual,
                        outcome,
                    } if actual == id => (true, outcome),
                    other => panic!(
                        "cartridge invariant: actor={} id={id} action={input:?} unexpected={other:?}",
                        self.actor
                    ),
                }
            }
            Pump::Completed {
                id: actual,
                outcome,
            } if actual == id => (false, outcome),
            other => panic!(
                "cartridge invariant: actor={} id={id} action={input:?} unexpected={other:?}",
                self.actor
            ),
        };
        // Per-call checks still run for completed actions in a partial time round.
        if let Ok(value) = &outcome {
            assert!(accepted, "successful operation was not admitted");
            let rows: [i64; 2] = serde_json::from_value(value.clone())
                .expect("cartridge result must contain two rows");
            assert_eq!(rows[0], rows[1], "cartridge paired rows diverged");
            if let Action::Change(edit) = &input.action {
                assert!(
                    matches!(edit.stop, Stop::Commit),
                    "failed handler published success"
                );
                assert_eq!(
                    rows,
                    [edit.expected + edit.amount; 2],
                    "success differs from the declared mutation"
                );
            } else {
                assert_eq!(
                    rows,
                    [self.model.borrow().value; 2],
                    "read differs from the last checked contention round"
                );
            }
        }
        let finished = self.stamp();
        self.result = Some(Attempt {
            action: input.action,
            accepted,
            outcome,
            issued,
            finished,
        });
    }
    fn check_round(actors: &mut [Self]) {
        let shared = actors[0].model.clone();
        let mut model = shared.borrow_mut();
        let attempts: Vec<_> = actors
            .iter_mut()
            .filter_map(|actor| actor.result.take())
            .collect();
        if model.verify {
            for attempt in attempts {
                assert!(matches!(attempt.action, Action::Read));
                assert!(attempt.accepted, "read was not admitted");
                assert_eq!(
                    attempt.outcome,
                    Ok(json!([model.value, model.value])),
                    "cartridge rows differ from the independent contention model"
                );
            }
        } else {
            let winners: Vec<_> = attempts
                .iter()
                .filter(|attempt| attempt.outcome.is_ok())
                .collect();
            assert!(
                winners.len() <= 1,
                "two commits passed the same compare guard"
            );
            let mut commit_candidates = 0;
            let mut rejected = 0;
            let mut switch_lower = winners.first().map_or(0, |winner| winner.issued);
            let mut switch_upper = winners.first().map_or(u64::MAX, |winner| winner.finished);
            for attempt in &attempts {
                let Action::Change(edit) = &attempt.action else {
                    panic!("mutation round contains a read");
                };
                if edit.expected == model.value && matches!(edit.stop, Stop::Commit) {
                    commit_candidates += 1;
                }
                if !attempt.accepted {
                    assert_eq!(
                        attempt.outcome,
                        Err(Error::Application(json!("stale"))),
                        "unexplained admission failure"
                    );
                    assert!(
                        edit.expected != model.value || !winners.is_empty(),
                        "fresh compare refused without another writer"
                    );
                    if edit.expected == model.value {
                        switch_upper = switch_upper.min(attempt.finished);
                    }
                } else {
                    assert_eq!(edit.expected, model.value, "stale compare was admitted");
                    switch_lower = switch_lower.max(attempt.issued);
                    if matches!(edit.stop, Stop::Commit) && attempt.outcome.is_err() {
                        assert_eq!(
                            attempt.outcome,
                            Err(Error::Unavailable),
                            "unexplained commit failure"
                        );
                        model.rejections.consume();
                        rejected += 1;
                    } else {
                        let mut value = model.value;
                        let (_, expected) = predict(&mut value, edit);
                        assert_eq!(
                            attempt.outcome, expected,
                            "handler outcome differs from its input contract"
                        );
                    }
                }
            }
            assert_eq!(
                winners.len(),
                usize::from(commit_candidates > rejected),
                "viable commits must make progress"
            );
            // A winning commit must fit inside its call interval, after all
            // admitted baseline comparisons can have run and before every fresh
            // stale refusal can have run. This also respects nonoverlapping calls
            // even when delivery order differs across physical peers.
            assert!(
                winners.is_empty() || switch_lower < switch_upper,
                "contention outcomes violate SDK call happens-before order"
            );
            if let Some(winner) = winners.first() {
                let Action::Change(edit) = &winner.action else {
                    unreachable!();
                };
                model.value = model
                    .value
                    .checked_add(edit.amount)
                    .expect("model overflow");
            }
        }
        model.verify = !model.verify;
    }
}
