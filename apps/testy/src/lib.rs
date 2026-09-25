//! Snap's transport/execution contract application. No Identity, Store or host dependencies.
#![no_std]
extern crate alloc;
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_transport::{Channel, Error, Value, json, server::Authority};

mod program;
pub use program::{App, CEILING};

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
fn start_output(value: &Value) -> bool {
    value.as_object().is_some_and(|obj| {
        obj.len() == 1 && obj.get("bearer").and_then(Value::as_str) == Some(BEARER)
    })
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
    /// Exercises a dependency read after a tentative mutation. The execution host
    /// supplies CEILING; a miss must roll back the entire attempt before retry.
    pub async fn add_checked(&mut self, operand: i64) -> Result<i64, Error> {
        self.calc("calc.add_checked", operand).await
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
