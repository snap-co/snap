use crate::{Channel, Command, Error, Event, Invocation, Outcome, Response, Value};
use alloc::string::String;

/// Correlation and lifecycle shared by every SDK/carrier. No credential discovery
/// operation names or Identity capability dependencies live here.
pub struct Client<C> {
    channel: C,
    sequence: u64,
}
impl<C: Channel> Client<C> {
    pub fn new(channel: C) -> Self {
        Self {
            channel,
            sequence: 0,
        }
    }
    pub fn replace_channel(&mut self, channel: C) {
        self.channel = channel;
    }
    pub async fn connect(&mut self, bearer: &str, client_id: &str) -> Result<bool, Error> {
        match self
            .channel
            .exchange(Command::Connect {
                bearer: bearer.into(),
                client_id: client_id.into(),
            })
            .await?
        {
            Response::Attached { resumed } => Ok(resumed),
            Response::Failed(error) => Err(error),
            _ => Err(Error::Protocol),
        }
    }
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        self.end(Command::Disconnect).await
    }
    pub async fn close(&mut self) -> Result<(), Error> {
        self.end(Command::Close).await
    }
    async fn end(&mut self, command: Command) -> Result<(), Error> {
        match self.channel.exchange(command).await? {
            Response::Detached => Ok(()),
            Response::Failed(error) => Err(error),
            _ => Err(Error::Protocol),
        }
    }
    pub async fn request(
        &mut self,
        bearer: Option<&str>,
        operation: &str,
        input: Value,
    ) -> Outcome {
        let invocation = self.invocation(operation, input)?;
        self.call(Command::Request {
            bearer: bearer.map(String::from),
            invocation,
        })
        .await
    }
    pub async fn invoke(&mut self, operation: &str, input: Value) -> Outcome {
        let invocation = self.invocation(operation, input)?;
        self.call(Command::Invoke(invocation)).await
    }
    fn invocation(&mut self, operation: &str, input: Value) -> Result<Invocation, Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        Ok(Invocation {
            id: self.sequence,
            operation: operation.into(),
            input,
        })
    }
    async fn call(&mut self, command: Command) -> Outcome {
        let response = self.channel.exchange(command).await?;
        match response {
            Response::Events(events) => match events.as_slice() {
                [
                    Event::Accepted { id },
                    Event::Completed {
                        id: completed,
                        outcome,
                    },
                ] if *id == self.sequence && id == completed => outcome.clone(),
                [
                    Event::Completed {
                        id,
                        outcome: Err(error),
                    },
                ] if *id == self.sequence => Err(error.clone()),
                _ => Err(Error::Protocol),
            },
            Response::Failed(error) => Err(error),
            _ => Err(Error::Protocol),
        }
    }
}
