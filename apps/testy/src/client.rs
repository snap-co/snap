use crate::{Calculator, start_output};
use alloc::string::String;
use snap_transport::{Channel, Error, Value, json};

pub struct Client<C> {
    transport: snap_transport::client::Client<C>,
    bearer: Option<String>,
    connected: bool,
}
impl<C: Channel> Client<C> {
    pub fn new(channel: C) -> Self {
        Self {
            transport: snap_transport::client::Client::new(channel),
            bearer: None,
            connected: false,
        }
    }
    pub async fn health(&mut self) -> Result<Value, Error> {
        let value = self
            .transport
            .request(None, "health.up", Value::Null)
            .await?;
        if value != json!({"status": "OK"}) {
            return Err(Error::InvalidOutput);
        }
        Ok(value)
    }
    /// Select credentials on an unattached carrier. Replacing a live connection's
    /// credentials would not change the identity already bound to that connection.
    pub fn use_session(&mut self, bearer: &str) -> Result<(), Error> {
        if self.connected {
            return Err(Error::Occupied);
        }
        self.bearer = Some(bearer.into());
        self.transport.use_bearer(bearer);
        Ok(())
    }
    pub async fn authenticate(
        &mut self,
        enroll: bool,
        email: &str,
        password: &str,
    ) -> Result<String, Error> {
        if self.connected {
            return Err(Error::Occupied);
        }
        let mut identity = snap_identity::client::Client::new(&mut self.transport);
        if enroll {
            identity.enroll(email, password).await?;
        } else {
            identity.acquire(email, password).await?;
        }
        let bearer: String = self.transport.bearer().ok_or(Error::InvalidOutput)?.into();
        self.bearer = Some(bearer);
        Ok(self.bearer.clone().unwrap())
    }
    pub async fn logout(&mut self) -> Result<(), Error> {
        snap_identity::client::Client::new(&mut self.transport)
            .release(snap_identity::ReleaseScope::Current)
            .await?;
        self.bearer = None;
        self.connected = false;
        Ok(())
    }
    /// Attach with an explicitly acquired session, then initialize connection state.
    pub async fn start(&mut self, client_id: &str) -> Result<(), Error> {
        self.transport
            .connect(
                self.bearer.as_deref().ok_or(Error::IdentityRequired)?,
                client_id,
            )
            .await?;
        self.connected = true;
        match self.transport.invoke("calc.start", Value::Null).await {
            Ok(value) if start_output(&value) => {}
            Err(Error::Application(value)) if value == json!("AlreadyStarted") => {}
            Ok(_) => return Err(Error::InvalidOutput),
            Err(error) => return Err(error),
        }
        Ok(())
    }
    pub async fn reconnect(&mut self, client_id: &str) -> Result<bool, Error> {
        let resumed = self
            .transport
            .connect(
                self.bearer.as_deref().ok_or(Error::IdentityRequired)?,
                client_id,
            )
            .await?;
        self.connected = true;
        Ok(resumed)
    }
    pub fn replace_channel(&mut self, channel: C) {
        self.transport.replace_channel(channel);
        self.connected = false;
    }
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        self.transport.disconnect().await?;
        self.connected = false;
        Ok(())
    }
    pub async fn close(&mut self) -> Result<(), Error> {
        self.transport.close().await?;
        self.connected = false;
        Ok(())
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
