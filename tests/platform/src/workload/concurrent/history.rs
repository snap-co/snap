//! Online single-lane history checker. Inputs create possible transitions; SDK
//! observations constrain them. No host instrumentation supplies state or order.
use super::Rejections;
use crate::{cartridge::Stop, workload::Action};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec,
    vec::Vec,
};

// Exhaustion is a campaign failure, never permission to discard possibilities or
// insert a client barrier. These limits bound ambiguous histories, not run length.
const MAX_CALLS: usize = 1_024;
const MAX_STATES: usize = 8_192;
const MAX_SEARCH: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ResultKind {
    Success(i64),
    Stale,
    Declined,
    Invalid,
    Miss,
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    // State-neutral executions that could precede an already observed admission.
    // Keeping their alternatives independently avoids a Cartesian product of
    // unobserved reads/rollbacks that do not change each other's results.
    Pending(Vec<ResultKind>),
    Admitted,
    Done(Vec<ResultKind>),
    Forgotten,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    value: i64,
    stages: Vec<Stage>,
}
struct Call {
    token: u64,
    action: Action,
    unknown: bool,
}
pub(super) struct History {
    calls: Vec<Call>,
    states: BTreeSet<State>,
    sequence: u64,
    last_singleton: i64,
    pub rejections: Rejections,
}
#[derive(Clone, Copy, Debug)]
enum Observation {
    Accepted,
    Completed(ResultKind),
}
impl Observation {
    fn matches(self, stage: &Stage) -> bool {
        match self {
            Self::Accepted => match stage {
                Stage::Admitted => true,
                Stage::Done(choices) => !choices.contains(&ResultKind::Stale),
                Stage::Pending(_) | Stage::Forgotten => false,
            },
            Self::Completed(result) => {
                matches!(stage, Stage::Done(choices) if choices.contains(&result))
            }
        }
    }
}
impl History {
    pub fn new(value: i64, rejections: Rejections) -> Self {
        Self {
            calls: Vec::new(),
            states: BTreeSet::from([State {
                value,
                stages: Vec::new(),
            }]),
            sequence: 0,
            last_singleton: value,
            rejections,
        }
    }
    pub fn issue(&mut self, action: Action) -> u64 {
        if let Action::Change(edit) = &action {
            assert!(edit.amount > 0, "history requires positive mutations");
            edit.expected
                .checked_add(edit.amount)
                .expect("model overflow");
        }
        assert!(
            self.calls.len() < MAX_CALLS,
            "history call capacity exhausted"
        );
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("history token overflow");
        self.calls.push(Call {
            token: self.sequence,
            action,
            unknown: false,
        });
        self.states = core::mem::take(&mut self.states)
            .into_iter()
            .map(|mut state| {
                state.stages.push(Stage::Pending(Vec::new()));
                state
            })
            .collect();
        self.sequence
    }
    fn index(&self, token: u64) -> usize {
        self.calls
            .iter()
            .position(|call| call.token == token)
            .expect("missing history call")
    }
    pub fn accepted(&mut self, token: u64) {
        self.observe(token, Observation::Accepted);
    }
    pub fn completed(&mut self, token: u64, result: ResultKind) {
        self.observe(token, Observation::Completed(result));
        let index = self.index(token);
        self.remove(&[index]);
        self.prune();
    }
    pub fn lost(&mut self, token: u64) {
        let index = self.index(token);
        self.calls[index].unknown = true;
        self.prune();
    }
    fn observe(&mut self, token: u64, observation: Observation) {
        let target = self.index(token);
        let mut todo: Vec<_> = self.states.iter().cloned().collect();
        let mut visited = BTreeSet::new();
        let mut next = BTreeSet::new();
        while let Some(mut state) = todo.pop() {
            self.normalize(&mut state);
            if !visited.insert(state.clone()) {
                continue;
            }
            assert!(
                visited.len() <= MAX_SEARCH,
                "history search capacity exhausted"
            );
            if observation.matches(&state.stages[target]) {
                next.insert(state);
                if next.len() > MAX_STATES {
                    next = Self::compact(next);
                }
                assert!(next.len() <= MAX_STATES, "history state capacity exhausted");
                continue;
            }
            if matches!(state.stages[target], Stage::Done(_)) {
                continue;
            }
            let lane = state
                .stages
                .iter()
                .position(|stage| *stage == Stage::Admitted);
            if matches!(observation, Observation::Accepted)
                && lane.is_none()
                && !matches!(&self.calls[target].action, Action::Change(edit) if matches!(edit.stop, Stop::Commit))
                && let Stage::Pending(prior) = &state.stages[target]
            {
                // A state-neutral admitted call can finish immediately. Its
                // eventual frame may be observed much later. Union earlier and
                // current outputs within this history instead of branching for
                // every outstanding reader's polling lag.
                let mut choices: Vec<_> = prior
                    .iter()
                    .copied()
                    .filter(|choice| *choice != ResultKind::Stale)
                    .collect();
                if let Some(result) = self.neutral_result(&self.calls[target].action, state.value)
                    && result != ResultKind::Stale
                    && !choices.contains(&result)
                {
                    choices.push(result);
                    choices.sort();
                }
                if !choices.is_empty() {
                    assert!(
                        choices.len() <= MAX_CALLS,
                        "history output capacity exhausted"
                    );
                    let mut prior = state.clone();
                    prior.stages[target] = Stage::Done(choices);
                    todo.push(prior);
                }
                self.predecessors(&state, Some(target), &mut todo);
                continue;
            }
            if let Stage::Pending(choices) = &state.stages[target] {
                let choices: Vec<_> = choices
                    .iter()
                    .copied()
                    .filter(|choice| match observation {
                        Observation::Accepted => *choice != ResultKind::Stale,
                        Observation::Completed(result) => *choice == result,
                    })
                    .collect();
                if !choices.is_empty() {
                    let mut prior = state.clone();
                    prior.stages[target] = Stage::Done(choices);
                    todo.push(prior);
                }
                assert!(todo.len() <= MAX_SEARCH, "history queue capacity exhausted");
            }
            if let Some(lane) = lane {
                self.finish(&state, lane, &mut todo);
                continue;
            }
            // Explicitly order value-changing predecessors. State-neutral calls
            // remember prior outputs independently, since one client's polling
            // can lag behind another client's observation of a later commit.
            let mut admitted = state.clone();
            self.remember(&mut admitted);
            admitted.stages[target] = match &self.calls[target].action {
                Action::Change(edit) if edit.expected != state.value => {
                    Stage::Done(vec![ResultKind::Stale])
                }
                _ => Stage::Admitted,
            };
            todo.push(admitted);
            self.predecessors(&state, Some(target), &mut todo);
        }
        assert!(
            !next.is_empty(),
            "history violates SDK happens-before order or input-derived possible states; fresh compare refused without another writer; token={token} observation={observation:?} action={:?} prior_states={}",
            self.calls[target].action,
            self.states.len()
        );
        self.states = Self::compact(next);
    }
    fn compact(states: BTreeSet<State>) -> BTreeSet<State> {
        // Remove only subsumed histories. Never union alternatives from different
        // histories: that could invent a path combining incompatible read values.
        let mut groups: BTreeMap<(i64, Vec<u8>), Vec<State>> = BTreeMap::new();
        for state in states {
            let shape = state
                .stages
                .iter()
                .map(|stage| match stage {
                    Stage::Pending(_) => 0,
                    Stage::Admitted => 1,
                    Stage::Done(_) => 2,
                    Stage::Forgotten => 3,
                })
                .collect();
            let group = groups.entry((state.value, shape)).or_default();
            if group.iter().any(|prior| Self::subsumes(prior, &state)) {
                continue;
            }
            group.retain(|prior| !Self::subsumes(&state, prior));
            group.push(state);
        }
        groups.into_values().flatten().collect()
    }
    fn subsumes(a: &State, b: &State) -> bool {
        a.stages.iter().zip(&b.stages).all(|(a, b)| match (a, b) {
            (Stage::Pending(a), Stage::Pending(b)) | (Stage::Done(a), Stage::Done(b)) => {
                b.iter().all(|choice| a.contains(choice))
            }
            _ => a == b,
        })
    }
    fn finish(&self, state: &State, lane: usize, todo: &mut Vec<State>) {
        let mut next = state.clone();
        let result = match &self.calls[lane].action {
            Action::Read => ResultKind::Success(state.value),
            Action::Change(edit) => {
                assert_eq!(
                    edit.expected, state.value,
                    "admitted lane changed underneath its guard"
                );
                match edit.stop {
                    Stop::Commit => {
                        if self.rejections.0.get() > 0 {
                            let mut rejected = state.clone();
                            rejected.stages[lane] = Stage::Done(vec![ResultKind::Rejected]);
                            todo.push(rejected);
                        }
                        next.value = edit
                            .expected
                            .checked_add(edit.amount)
                            .expect("model overflow");
                        ResultKind::Success(next.value)
                    }
                    Stop::Application => ResultKind::Declined,
                    Stop::InvalidOutput => ResultKind::Invalid,
                    Stop::CaughtMiss => ResultKind::Miss,
                }
            }
        };
        next.stages[lane] = Stage::Done(vec![result]);
        todo.push(next);
        assert!(todo.len() <= MAX_SEARCH, "history queue capacity exhausted");
    }
    fn predecessors(&self, state: &State, target: Option<usize>, todo: &mut Vec<State>) {
        for (index, call) in self.calls.iter().enumerate() {
            if Some(index) == target || !matches!(state.stages[index], Stage::Pending(_)) {
                continue;
            }
            let Action::Change(edit) = &call.action else {
                continue;
            };
            if edit.expected != state.value || !matches!(edit.stop, Stop::Commit) {
                continue;
            }
            let mut next = state.clone();
            self.remember(&mut next);
            next.value = edit
                .expected
                .checked_add(edit.amount)
                .expect("model overflow");
            next.stages[index] = Stage::Done(vec![ResultKind::Success(next.value)]);
            todo.push(next);
            assert!(todo.len() <= MAX_SEARCH, "history queue capacity exhausted");
        }
    }
    fn remember(&self, state: &mut State) {
        for (call, stage) in self.calls.iter().zip(&mut state.stages) {
            let Stage::Pending(choices) = stage else {
                continue;
            };
            let result = self.neutral_result(&call.action, state.value);
            if let Some(result) = result
                && !choices.contains(&result)
            {
                choices.push(result);
                choices.sort();
                assert!(
                    choices.len() <= MAX_CALLS,
                    "history output capacity exhausted"
                );
            }
        }
    }
    fn neutral_result(&self, action: &Action, value: i64) -> Option<ResultKind> {
        match action {
            Action::Read => Some(ResultKind::Success(value)),
            Action::Change(edit) if edit.expected != value => Some(ResultKind::Stale),
            Action::Change(edit) => match edit.stop {
                Stop::Application => Some(ResultKind::Declined),
                Stop::InvalidOutput => Some(ResultKind::Invalid),
                Stop::CaughtMiss => Some(ResultKind::Miss),
                Stop::Commit if self.rejections.0.get() > 0 => Some(ResultKind::Rejected),
                Stop::Commit => None,
            },
        }
    }
    fn remove(&mut self, indices: &[usize]) {
        for &index in indices.iter().rev() {
            self.calls.remove(index);
        }
        self.states = core::mem::take(&mut self.states)
            .into_iter()
            .map(|mut state| {
                for &index in indices.iter().rev() {
                    state.stages.remove(index);
                }
                state
            })
            .collect();
    }
    fn normalize(&self, state: &mut State) {
        for (call, stage) in self.calls.iter().zip(&mut state.stages) {
            if !call.unknown {
                continue;
            }
            let forget = match stage {
                Stage::Done(_) | Stage::Forgotten => true,
                Stage::Admitted => false,
                Stage::Pending(_) => match &call.action {
                    Action::Read => true,
                    Action::Change(edit) => {
                        edit.expected < state.value || !matches!(edit.stop, Stop::Commit)
                    }
                },
            };
            if forget {
                *stage = Stage::Forgotten;
            }
        }
    }
    fn prune(&mut self) {
        // Loss is not a commit deadline. Forget an unknown call only once every
        // surviving history has executed it, or its positive compare can no
        // longer succeed. Nonmutating, unadmitted lost calls can also disappear.
        self.states = core::mem::take(&mut self.states)
            .into_iter()
            .map(|mut state| {
                self.normalize(&mut state);
                state
            })
            .collect();
        let indices: Vec<_> = self
            .calls
            .iter()
            .enumerate()
            .filter_map(|(index, call)| {
                (call.unknown
                    && self
                        .states
                        .iter()
                        .all(|state| state.stages[index] == Stage::Forgotten))
                .then_some(index)
            })
            .collect();
        self.remove(&indices);
        if self.calls.is_empty() && self.states.len() == 1 {
            self.last_singleton = self.states.first().unwrap().value;
        }
    }
    /// Includes issued calls that can still run after their observation is lost.
    /// At a horizon this is a possibility set, not a drained final Store claim.
    pub fn values(&self) -> Vec<i64> {
        let mut todo: Vec<_> = self.states.iter().cloned().collect();
        let mut visited = BTreeSet::new();
        let mut values = BTreeSet::new();
        while let Some(mut state) = todo.pop() {
            self.normalize(&mut state);
            if !visited.insert(state.clone()) {
                continue;
            }
            assert!(
                visited.len() <= MAX_SEARCH,
                "history report capacity exhausted"
            );
            values.insert(state.value);
            if let Some(lane) = state
                .stages
                .iter()
                .position(|stage| *stage == Stage::Admitted)
            {
                self.finish(&state, lane, &mut todo);
            } else {
                self.predecessors(&state, None, &mut todo);
            }
        }
        values.into_iter().collect()
    }
    pub fn expected_value(&self) -> i64 {
        let values = self.values();
        if values.len() == 1 {
            values[0]
        } else {
            self.last_singleton
        }
    }
}
