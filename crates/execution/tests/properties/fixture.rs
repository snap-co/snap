//! A tiny application owned by execution tests. Every attempt mutates both its
//! balance and history before asking for reads. It retains no invocation state.
use snap_execution::*;

pub struct Ledger {
    pub factor: i64,
    pub version: u64,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            factor: 1,
            version: 1,
        }
    }
}

static OPERATIONS: [Operation; 1] = [Operation {
    key: "apply",
    identity_required: true,
    input: |value| value["delta"].as_i64().is_some() && value["reads"].as_u64().is_some(),
    output: |value| value.as_i64().is_some(),
    error: |value| value == "denied",
}];

impl Program for Ledger {
    fn state_version(&self) -> u64 {
        self.version
    }
    fn valid_state(&self, state: &Value) -> bool {
        state.is_null()
            || (state["balance"].as_i64().is_some() && state["history"].as_array().is_some())
    }
    fn operations(&self) -> &[Operation] {
        &OPERATIONS
    }
    fn admit(&self, call: &Call, view: View<'_>) -> Admission {
        if call.input["guard"] != true {
            return Admission::Ready;
        }
        match view.inputs.read("guard") {
            Err(Stop::Need(key)) => Admission::Need(key),
            Ok(value) if value == true => Admission::Ready,
            _ => Admission::Reject(Error::Application(json!("denied"))),
        }
    }
    fn attempt(&self, call: &Call, mut work: WorkingSet<'_>) -> Attempt {
        let delta = call.input["delta"].as_i64().unwrap() * self.factor;
        let balance = work.state["balance"].as_i64().unwrap_or(0) + delta;
        let mut history = work.state["history"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        history.push(json!(delta));
        work.state = json!({"balance": balance, "history": history});
        for index in 0..call.input["reads"].as_u64().unwrap() {
            if let Err(Stop::Need(key)) = work.inputs.read(&format!("input-{index}")) {
                return Attempt::Need(key);
            }
        }
        match call.input["mode"].as_u64().unwrap_or(0) {
            1 => Attempt::Fail(Error::Application(json!("denied"))),
            2 => Attempt::Commit {
                state: work.state,
                result: Value::Null,
            },
            3 => Attempt::Commit {
                state: json!("invalid"),
                result: json!(balance),
            },
            4 => Attempt::Need("input-0".into()),
            _ => Attempt::Commit {
                state: work.state,
                result: json!(balance),
            },
        }
    }
}

pub fn call(delta: i64, reads: usize, mode: u8, guard: bool) -> Call {
    Call {
        operation: "apply".into(),
        identity: Some("verified".into()),
        input: json!({"delta": delta, "reads": reads, "mode": mode, "guard": guard}),
    }
}

pub fn state(history: &[i64]) -> Value {
    if history.is_empty() {
        Value::Null
    } else {
        json!({"balance": history.iter().sum::<i64>(), "history": history})
    }
}
