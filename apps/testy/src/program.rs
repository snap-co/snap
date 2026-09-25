use crate::{BEARER, Calculator, Entry, start_output};
use snap_execution::{
    Admission, Attempt, Call, Error, Operation, Program, Stop, Value, View, WorkingSet, json,
};

pub const CEILING: &str = "testy.calculator.ceiling";

pub struct App {
    operations: [Operation; 7],
    add: fn(i64, i64) -> Option<i64>,
}
impl Default for App {
    fn default() -> Self {
        Self::with_add(i64::checked_add)
    }
}
impl App {
    /// A code-selection fixture for the reload demonstration. The selected
    /// function belongs to the program, never to retained calculator data.
    pub fn with_add(add: fn(i64, i64) -> Option<i64>) -> Self {
        Self {
            operations: [
                Operation {
                    key: "calc.start",
                    identity_required: false,
                    input: Value::is_null,
                    output: start_output,
                    error: declared_error,
                },
                arithmetic("calc.add"),
                arithmetic("calc.sub"),
                arithmetic("calc.mul"),
                arithmetic("calc.div"),
                arithmetic("calc.add_checked"),
                Operation {
                    key: "calc.inspect",
                    identity_required: true,
                    input: Value::is_null,
                    output: calculator_output,
                    error: declared_error,
                },
            ],
            add,
        }
    }
    fn handle(&self, call: &Call, work: &mut WorkingSet<'_>) -> Result<Value, Stop> {
        if call.operation == "calc.start" {
            if work.connected {
                if !work.state.is_null() {
                    return Err(failure("AlreadyStarted").into());
                }
                work.state = json!(Calculator::default());
            }
            return Ok(json!({"bearer": BEARER}));
        }
        if call.operation == "calc.inspect" {
            return Ok(work.state.clone());
        }
        let operand = call.input.as_i64().ok_or(Error::InvalidInput)?;
        let mut calculator: Calculator =
            serde_json::from_value(work.state.clone()).map_err(|_| Error::InvalidState)?;
        if calculator.history.len() >= 128 {
            return Err(failure("HistoryFull").into());
        }
        if call.operation == "calc.div" && operand == 0 {
            return Err(failure("DivisionByZero").into());
        }
        let before = calculator.accumulator;
        let after = match call.operation.as_str() {
            "calc.add" | "calc.add_checked" => (self.add)(before, operand),
            "calc.sub" => before.checked_sub(operand),
            "calc.mul" => before.checked_mul(operand),
            "calc.div" => before.checked_div(operand),
            _ => return Err(Error::UnknownOperation.into()),
        }
        .ok_or_else(|| failure("Overflow"))?;
        calculator.accumulator = after;
        calculator.history.push(Entry {
            operation: call.operation.clone(),
            operand,
            before,
            after,
        });
        work.state = json!(calculator);
        if call.operation == "calc.add_checked" {
            // Intentionally read after the private write. Missing input unwinds
            // the attempt; the host still has the pre-attempt calculator/history.
            let ceiling = work
                .inputs
                .read(CEILING)?
                .as_i64()
                .ok_or(Error::InvalidInput)?;
            if after > ceiling {
                return Err(failure("AboveCeiling").into());
            }
        }
        Ok(json!(after))
    }
}
impl Program for App {
    fn state_version(&self) -> u64 {
        1
    }
    fn valid_state(&self, state: &Value) -> bool {
        state.is_null() || calculator_output(state)
    }
    fn operations(&self) -> &[Operation] {
        &self.operations
    }
    fn admit(&self, call: &Call, view: View<'_>) -> Admission {
        if call.operation != "calc.start" && (!view.connected || view.state.is_null()) {
            Admission::Reject(failure("NotStarted"))
        } else {
            Admission::Ready
        }
    }
    fn attempt(&self, call: &Call, mut work: WorkingSet<'_>) -> Attempt {
        let result = self.handle(call, &mut work);
        work.finish(result)
    }
}
fn arithmetic(key: &'static str) -> Operation {
    Operation {
        key,
        identity_required: true,
        input: integer,
        output: integer,
        error: declared_error,
    }
}
fn integer(value: &Value) -> bool {
    value.as_i64().is_some()
}
fn calculator_output(value: &Value) -> bool {
    serde_json::from_value::<Calculator>(value.clone()).is_ok_and(|calc| calc.history.len() <= 128)
}
fn declared_error(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some(
            "NotStarted"
                | "DivisionByZero"
                | "Overflow"
                | "AlreadyStarted"
                | "HistoryFull"
                | "AboveCeiling"
        )
    )
}
fn failure(message: &str) -> Error {
    Error::Application(json!(message))
}
