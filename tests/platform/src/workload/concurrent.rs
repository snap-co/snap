//! Bounded contention oracle for the two-row cartridge. Each mutation round uses
//! one baseline compare and positive, actor-distinct increments. Consequently at
//! most one commit can win. Observations select that possible ordering, but never
//! supply expected row values: the model computes the winner's value from INPUTS.
//! Lost calls stay unknown. Possible states come from issued inputs, then fresh
//! verification reads constrain that set. No uncertain mutation is replayed.
use super::{Action, World};
use crate::{
    cartridge::{Change, Edit, Read, Stop},
    dispatch::predict,
    runner::{NoReconnect, Random, Reconnect, Timer, Workload},
};
use alloc::{rc::Rc, string::String, vec, vec::Vec};
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
    possible: Vec<i64>,
    verify: bool,
    rejections: Rejections,
    sequence: u64,
}
struct Attempt {
    action: Action,
    accepted: bool,
    outcome: Option<Outcome>,
    issued: u64,
    finished: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Input {
    pub delay_ms: u64,
    pub action: Action,
}
#[derive(Debug)]
pub struct Diagnostic {
    pub actor: usize,
    pub action_index: u64,
    pub id: Option<u64>,
    pub input: Input,
    pub phase: &'static str,
}
pub struct Probe<C, T, R = NoReconnect> {
    client: Client<C>,
    timer: T,
    model: Rc<RefCell<Model>>,
    actor: usize,
    actors: usize,
    result: Option<Attempt>,
    recovery: R,
    client_id: String,
    unknown: u64,
    diagnostic: Option<Diagnostic>,
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
            possible: vec![world.value],
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
                recovery: NoReconnect,
                client_id: alloc::format!("campaign-client-{actor}"),
                unknown: 0,
                diagnostic: None,
            })
            .collect()
    }
    /// Replace the host factory without changing application policy or SDK state.
    pub fn recovering<R: Reconnect<C>>(self, recovery: R, client_id: String) -> Probe<C, T, R> {
        Probe {
            client: self.client,
            timer: self.timer,
            model: self.model,
            actor: self.actor,
            actors: self.actors,
            result: self.result,
            recovery,
            client_id,
            unknown: 0,
            diagnostic: None,
        }
    }
}
impl<C: Channel, T: Timer, R: Reconnect<C>> Probe<C, T, R> {
    /// Last singleton state, not a claim that an unresolved mutation failed.
    /// Reports and recovery callers should inspect possible_values instead.
    pub fn expected_value(&self) -> i64 {
        self.model.borrow().value
    }
    pub fn possible_values(&self) -> Vec<i64> {
        self.model.borrow().possible.clone()
    }
    pub fn unknown_calls(&self) -> u64 {
        self.unknown
    }
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        self.diagnostic.as_ref()
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
impl<C: Channel, T: Timer, R: Reconnect<C>> Workload for Probe<C, T, R> {
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
        let action_index = self
            .diagnostic
            .as_ref()
            .map_or(0, |diagnostic| diagnostic.action_index + 1);
        self.diagnostic = Some(Diagnostic {
            actor: self.actor,
            action_index,
            id: None,
            input: input.clone(),
            phase: "think-time",
        });
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
        self.diagnostic.as_mut().unwrap().id = Some(id);
        self.diagnostic.as_mut().unwrap().phase = "awaiting-admission";
        let mut accepted = false;
        let outcome = loop {
            match self.client.pump().await {
                Ok(Pump::Accepted { id: actual }) if actual == id && !accepted => {
                    accepted = true;
                    self.diagnostic.as_mut().unwrap().phase = "awaiting-completion";
                }
                Ok(Pump::Completed {
                    id: actual,
                    outcome,
                }) if actual == id => break Some(outcome),
                Err(Error::Unavailable) | Ok(Pump::Closed) => break None,
                other => panic!(
                    "cartridge invariant: {:?} unexpected={other:?}",
                    self.diagnostic
                ),
            }
        };
        // Per-call checks still run for completed actions in a partial time round.
        if let Some(Ok(value)) = &outcome {
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
                assert!(
                    self.model.borrow().possible.contains(&rows[0]),
                    "read differs from all input-derived possible states: {:?}",
                    self.diagnostic
                );
            }
        }
        let finished = self.stamp();
        if outcome.is_none() {
            assert!(
                matches!(input.action, Action::Change(_)),
                "verification reads require a fault-free recovery window"
            );
            self.client.abandon(id);
            assert_eq!(
                self.client.outstanding(),
                0,
                "lost trace must not survive recovery"
            );
            self.unknown += 1;
            self.diagnostic.as_mut().unwrap().phase = "opening-replacement";
            let channel = self
                .recovery
                .open()
                .await
                .expect("recovery carrier open failed");
            self.client.replace_channel(channel);
            self.diagnostic.as_mut().unwrap().phase = "reconnecting";
            self.client
                .connect("alice", &self.client_id)
                .await
                .expect("logical reconnect failed");
            // No begin/invoke here: fresh verification belongs to the next round.
        }
        self.diagnostic.as_mut().unwrap().phase = if outcome.is_none() {
            "unknown-reconnected"
        } else {
            "completed"
        };
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
            let mut observed = None;
            for attempt in attempts {
                assert!(matches!(attempt.action, Action::Read));
                assert!(attempt.accepted, "read was not admitted");
                let rows: [i64; 2] = serde_json::from_value(
                    attempt
                        .outcome
                        .expect("verification lost its outcome")
                        .expect("verification failed"),
                )
                .unwrap();
                assert_eq!(rows[0], rows[1], "verification found torn rows");
                assert!(
                    model.possible.contains(&rows[0]),
                    "cartridge rows differ from all input-derived possible states"
                );
                if let Some(value) = observed {
                    assert_eq!(rows[0], value, "verification peers disagree");
                }
                observed = Some(rows[0]);
            }
            model.value = observed.expect("verification round requires a read");
            model.possible = vec![model.value];
        } else {
            assert_eq!(
                model.possible,
                [model.value],
                "mutation must follow a resolved verification round"
            );
            for attempt in &attempts {
                let Action::Change(edit) = &attempt.action else {
                    panic!("mutation round contains a read");
                };
                let Some(outcome) = &attempt.outcome else {
                    if attempt.accepted {
                        assert_eq!(edit.expected, model.value, "stale compare was admitted");
                    }
                    continue;
                };
                if !attempt.accepted {
                    assert_eq!(
                        outcome,
                        &Err(Error::Application(json!("stale"))),
                        "unexplained admission failure"
                    );
                } else {
                    assert_eq!(edit.expected, model.value, "stale compare was admitted");
                    if matches!(edit.stop, Stop::Commit) && outcome.is_err() {
                        assert_eq!(
                            outcome,
                            &Err(Error::Unavailable),
                            "unexplained commit failure"
                        );
                        model.rejections.consume();
                    } else {
                        let mut value = model.value;
                        let (_, expected) = predict(&mut value, edit);
                        assert_eq!(
                            outcome, &expected,
                            "handler outcome differs from its input contract"
                        );
                    }
                }
            }
            // Enumerate no commit or exactly one eligible issued mutation. An
            // unknown call may not have reached admission, or may commit after
            // its observation channel dies. Loss time is NOT its commit deadline.
            let mut possible = Vec::new();
            for winner in core::iter::once(None).chain(
                attempts
                    .iter()
                    .filter(|attempt| {
                        let Action::Change(edit) = &attempt.action else {
                            unreachable!();
                        };
                        edit.expected == model.value
                            && matches!(edit.stop, Stop::Commit)
                            && attempt.outcome.as_ref().is_none_or(Result::is_ok)
                    })
                    .map(Some),
            ) {
                let mut lower = winner.map_or(0, |attempt| attempt.issued);
                let mut upper = winner.map_or(u64::MAX, |attempt| {
                    if attempt.outcome.is_some() {
                        attempt.finished
                    } else {
                        u64::MAX
                    }
                });
                let mut valid = true;
                for attempt in &attempts {
                    let Action::Change(edit) = &attempt.action else {
                        unreachable!();
                    };
                    if attempt.accepted {
                        lower = lower.max(attempt.issued);
                    }
                    if let Some(outcome) = &attempt.outcome {
                        if outcome.is_ok() {
                            valid &=
                                winner.is_some_and(|candidate| core::ptr::eq(candidate, attempt));
                        }
                        if !attempt.accepted && edit.expected == model.value {
                            valid &= winner.is_some();
                            upper = upper.min(attempt.finished);
                        }
                    }
                }
                valid &= winner.is_none() || lower < upper;
                if valid {
                    let value = winner.map_or(model.value, |attempt| {
                        let Action::Change(edit) = &attempt.action else {
                            unreachable!();
                        };
                        model
                            .value
                            .checked_add(edit.amount)
                            .expect("model overflow")
                    });
                    if !possible.contains(&value) {
                        possible.push(value);
                    }
                }
            }
            assert!(
                !possible.is_empty(),
                "contention outcomes violate SDK call happens-before order or fresh compare refused without another writer"
            );
            model.possible = possible;
            if model.possible.len() == 1 {
                model.value = model.possible[0];
            }
        }
        model.verify = !model.verify;
    }
}
