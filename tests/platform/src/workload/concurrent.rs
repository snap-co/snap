//! Independently driven cartridge clients with a bounded online history oracle.
//! Each actor alternates its own mutation and read, without waiting for peers.
//! Lost mutations are never replayed and may execute after physical recovery.
mod history;
use super::{Action, World};
use crate::{
    cartridge::{Change, Read, Stop},
    runner::{NoReconnect, Random, Reconnect, Timer, Workload},
};
use alloc::{rc::Rc, string::String, vec::Vec};
use core::cell::{Cell, RefCell};
use history::{History, ResultKind};
use serde::Serialize;
use snap_transport::{
    Channel, Error, Operation,
    client::{Client, Pump},
    json,
};

/// Credits come from fault inputs, not server outcomes. An observed confirmed
/// rejection consumes a credit. Coalesced injections may leave unused credits.
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
    model: Rc<RefCell<History>>,
    actor: usize,
    actors: usize,
    value: i64,
    verify: bool,
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
        let model = Rc::new(RefCell::new(History::new(world.value, rejections)));
        clients
            .into_iter()
            .enumerate()
            .map(|(actor, client)| Self {
                client,
                timer: timer.clone(),
                model: model.clone(),
                actor,
                actors,
                value: world.value,
                verify: false,
                recovery: NoReconnect,
                client_id: alloc::format!("campaign-client-{actor}"),
                unknown: 0,
                diagnostic: None,
            })
            .collect()
    }
}
impl<C: Channel, T: Timer, R: Reconnect<C>> Probe<C, T, R> {
    /// Replace physical connection assembly without resetting the SDK or history.
    pub fn recovering<N: Reconnect<C>>(self, recovery: N, client_id: String) -> Probe<C, T, N> {
        Probe {
            client: self.client,
            timer: self.timer,
            model: self.model,
            actor: self.actor,
            actors: self.actors,
            value: self.value,
            verify: self.verify,
            recovery,
            client_id,
            unknown: self.unknown,
            diagnostic: self.diagnostic,
        }
    }
    /// Last singleton, not a claim that unresolved or horizon-stopped calls failed.
    pub fn expected_value(&self) -> i64 {
        self.model.borrow().expected_value()
    }
    pub fn possible_values(&self) -> Vec<i64> {
        self.model.borrow().values()
    }
    pub fn unknown_calls(&self) -> u64 {
        self.unknown
    }
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        self.diagnostic.as_ref()
    }
}
impl<C: Channel, T: Timer, R: Reconnect<C>> Workload for Probe<C, T, R> {
    type Action = Input;
    fn generate(&mut self, random: &mut Random) -> Input {
        let action = if self.verify {
            Action::Read
        } else {
            Action::Change(crate::cartridge::Edit {
                expected: if random.below(8) == 0 {
                    self.value - 1
                } else {
                    self.value
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
        let token = self.model.borrow_mut().issue(input.action.clone());
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
                    self.model.borrow_mut().accepted(token);
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
        if let Some(outcome) = &outcome {
            let result = match outcome {
                Ok(value) => {
                    assert!(accepted, "successful operation was not admitted");
                    let rows: [i64; 2] = serde_json::from_value(value.clone())
                        .expect("cartridge result must contain two rows");
                    assert_eq!(rows[0], rows[1], "cartridge paired rows diverged");
                    ResultKind::Success(rows[0])
                }
                Err(Error::Application(value)) if !accepted && *value == json!("stale") => {
                    ResultKind::Stale
                }
                Err(Error::Application(value)) if accepted && *value == json!("declined") => {
                    ResultKind::Declined
                }
                Err(Error::Application(value))
                    if accepted && *value == json!({"code":"StoreMiss"}) =>
                {
                    ResultKind::Miss
                }
                Err(Error::InvalidOutput) if accepted => ResultKind::Invalid,
                Err(Error::Unavailable) if accepted => ResultKind::Rejected,
                other => panic!(
                    "unexplained operation outcome: {other:?}; {:?}",
                    self.diagnostic
                ),
            };
            self.model.borrow_mut().completed(token, result);
            if result == ResultKind::Rejected {
                self.model.borrow().rejections.consume();
            }
            if let ResultKind::Success(value) = result {
                // Observations guide future INPUTS only after the independent
                // history validates them. They never supply expected model state.
                self.value = value;
            }
        } else {
            assert!(
                matches!(input.action, Action::Change(_)),
                "verification reads require a fault-free recovery window"
            );
            self.model.borrow_mut().lost(token);
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
        }
        self.verify = matches!(input.action, Action::Change(_));
        self.diagnostic.as_mut().unwrap().phase = if outcome.is_none() {
            "unknown-reconnected"
        } else {
            "completed"
        };
    }
}
