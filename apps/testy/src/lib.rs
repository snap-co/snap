//! Snap's transport contract application. No Identity, Store or host dependencies.
#![no_std]
extern crate alloc;
use alloc::{rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;
use serde::{Deserialize, Serialize};
use snap_transport::{
    Channel, Error, Outcome, Value, json,
    server::{Application, Authority, Context, Operation},
};

pub const BEARER: &str = "testy-private-fixture-token";
pub const IDENTITY: &str = "testy-fixture-identity";
pub struct TestAuthority;
impl Authority for TestAuthority {
    fn identify(&self, bearer: &str) -> Option<String> {
        (bearer == BEARER).then(|| IDENTITY.into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub operation: String,
    pub operand: i64,
    pub before: i64,
    pub after: i64,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Calculator {
    pub accumulator: i64,
    pub history: Vec<Entry>,
}
#[derive(Default)]
pub struct State {
    pub calculator: Option<Rc<RefCell<Calculator>>>,
}

pub struct App {
    operations: [Operation<State>; 6],
}
impl Default for App {
    fn default() -> Self {
        Self {
            operations: [
                Operation {
                    key: "calc.start",
                    identity_required: false,
                    input: null,
                    output: start_output,
                    error: declared_error,
                    guard: allow,
                    handle: start,
                },
                arithmetic("calc.add", add),
                arithmetic("calc.sub", sub),
                arithmetic("calc.mul", mul),
                arithmetic("calc.div", div),
                Operation {
                    key: "calc.inspect",
                    identity_required: true,
                    input: null,
                    output: calculator_output,
                    error: declared_error,
                    guard: ready,
                    handle: inspect,
                },
            ],
        }
    }
}
impl Application for App {
    type State = State;
    fn operations(&self) -> &[Operation<State>] {
        &self.operations
    }
}
fn arithmetic(
    key: &'static str,
    handle: fn(&Context, &mut State, Value) -> Outcome,
) -> Operation<State> {
    Operation {
        key,
        identity_required: true,
        input: integer,
        output: integer,
        error: declared_error,
        guard: ready,
        handle,
    }
}
fn null(value: &Value) -> bool {
    value.is_null()
}
fn integer(value: &Value) -> bool {
    value.as_i64().is_some()
}
fn start_output(value: &Value) -> bool {
    value.as_object().is_some_and(|obj| {
        obj.len() == 1 && obj.get("bearer").and_then(Value::as_str) == Some(BEARER)
    })
}
fn calculator_output(value: &Value) -> bool {
    serde_json::from_value::<Calculator>(value.clone()).is_ok()
}
fn declared_error(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("NotStarted" | "DivisionByZero" | "Overflow" | "AlreadyStarted" | "HistoryFull")
    )
}
fn failure(message: &str) -> Error {
    Error::Application(json!(message))
}
fn allow(_: &Context, _: &State, _: &Value) -> Result<(), Error> {
    Ok(())
}
fn ready(context: &Context, state: &State, _: &Value) -> Result<(), Error> {
    if !context.connected || state.calculator.is_none() {
        Err(failure("NotStarted"))
    } else {
        Ok(())
    }
}
fn start(context: &Context, state: &mut State, _: Value) -> Outcome {
    if context.connected {
        if state.calculator.is_some() {
            return Err(failure("AlreadyStarted"));
        }
        state.calculator = Some(Rc::default());
    }
    Ok(json!({"bearer": BEARER}))
}
fn inspect(_: &Context, state: &mut State, _: Value) -> Outcome {
    serde_json::to_value(&*state.calculator.as_ref().unwrap().borrow())
        .map_err(|_| Error::InvalidOutput)
}
fn calculate(
    state: &mut State,
    input: Value,
    key: &str,
    operation: fn(i64, i64) -> Option<i64>,
) -> Outcome {
    let operand = input.as_i64().unwrap();
    let mut calculator = state.calculator.as_ref().unwrap().borrow_mut();
    if calculator.history.len() >= 128 {
        return Err(failure("HistoryFull"));
    }
    if key == "calc.div" && operand == 0 {
        return Err(failure("DivisionByZero"));
    }
    let before = calculator.accumulator;
    let after = operation(before, operand).ok_or_else(|| failure("Overflow"))?;
    calculator.history.push(Entry {
        operation: key.into(),
        operand,
        before,
        after,
    });
    calculator.accumulator = after;
    Ok(json!(after))
}
fn add(_: &Context, state: &mut State, input: Value) -> Outcome {
    calculate(state, input, "calc.add", i64::checked_add)
}
fn sub(_: &Context, state: &mut State, input: Value) -> Outcome {
    calculate(state, input, "calc.sub", i64::checked_sub)
}
fn mul(_: &Context, state: &mut State, input: Value) -> Outcome {
    calculate(state, input, "calc.mul", i64::checked_mul)
}
fn div(_: &Context, state: &mut State, input: Value) -> Outcome {
    calculate(state, input, "calc.div", i64::checked_div)
}

pub struct Client<C> {
    transport: snap_transport::client::Client<C>,
}
impl<C: Channel> Client<C> {
    pub fn new(channel: C) -> Self {
        Self {
            transport: snap_transport::client::Client::new(channel),
        }
    }
    /// Bootstrap is deliberately explicit: anonymous credential acquisition,
    /// authenticated attachment, then initialization of connection-owned state.
    pub async fn start(&mut self, client_id: &str) -> Result<(), Error> {
        let result = self
            .transport
            .request(None, "calc.start", Value::Null)
            .await?;
        if !start_output(&result) {
            return Err(Error::InvalidOutput);
        }
        self.transport
            .connect(result["bearer"].as_str().unwrap(), client_id)
            .await?;
        match self.transport.invoke("calc.start", Value::Null).await {
            Ok(value) if start_output(&value) => {}
            Err(Error::Application(value)) if value == json!("AlreadyStarted") => {}
            Ok(_) => return Err(Error::InvalidOutput),
            Err(error) => return Err(error),
        }
        Ok(())
    }
    pub async fn reconnect(&mut self, client_id: &str) -> Result<bool, Error> {
        self.transport.connect(BEARER, client_id).await
    }
    pub fn replace_channel(&mut self, channel: C) {
        self.transport.replace_channel(channel);
    }
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        self.transport.disconnect().await
    }
    pub async fn close(&mut self) -> Result<(), Error> {
        self.transport.close().await
    }
    pub async fn add(&mut self, operand: i64) -> Result<i64, Error> {
        self.calc("calc.add", operand).await
    }
    pub async fn sub(&mut self, operand: i64) -> Result<i64, Error> {
        self.calc("calc.sub", operand).await
    }
    pub async fn mul(&mut self, operand: i64) -> Result<i64, Error> {
        self.calc("calc.mul", operand).await
    }
    pub async fn div(&mut self, operand: i64) -> Result<i64, Error> {
        self.calc("calc.div", operand).await
    }
    async fn calc(&mut self, key: &str, operand: i64) -> Result<i64, Error> {
        self.transport
            .invoke(key, json!(operand))
            .await?
            .as_i64()
            .ok_or(Error::InvalidOutput)
    }
    pub async fn inspect(&mut self) -> Result<Calculator, Error> {
        serde_json::from_value(self.transport.invoke("calc.inspect", Value::Null).await?)
            .map_err(|_| Error::InvalidOutput)
    }
}

/// A small SDK program shared unchanged by platform compositions.
pub async fn journey<C: Channel>(client: &mut Client<C>, id: &str) -> Result<Calculator, Error> {
    client.start(id).await?;
    client.add(12).await?;
    client.sub(2).await?;
    client.mul(3).await?;
    client.div(5).await?;
    client.inspect().await
}
