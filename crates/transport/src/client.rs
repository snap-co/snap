use crate::{Channel, Command, Error, Event, Invocation, Outcome, Response, Value};
use alloc::string::String;

/// Correlation and lifecycle shared by every SDK/carrier. No credential discovery
/// operation names or Identity capability dependencies live here.
pub struct Client<C> {
    channel: C,
    sequence: u64,
    bearer: Option<crate::bearer::Token>,
}
impl<C: Channel> Client<C> {
    pub fn new(channel: C) -> Self {
        Self {
            channel,
            sequence: 0,
            bearer: None,
        }
    }
    pub fn bearer(&self) -> Option<&str> {
        self.bearer.as_ref().map(crate::bearer::Token::expose)
    }
    pub fn use_bearer(&mut self, bearer: &str) {
        self.bearer = Some(crate::bearer::Token::new(bearer.into()));
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
        self.invoke_with_progress(operation, input, |_| {}).await
    }

    pub async fn invoke_with_progress(
        &mut self,
        operation: &str,
        input: Value,
        progress: impl FnMut(Value),
    ) -> Outcome {
        let invocation = self.invocation(operation, input)?;
        self.call_with_progress(Command::Invoke(invocation), progress)
            .await
    }

    pub async fn invoke_typed<O: crate::Operation>(
        &mut self,
        input: &O::Input,
        mut progress: impl FnMut(O::Progress),
    ) -> Result<O::Output, crate::Failure<O::Error>> {
        let input = serde_json::to_value(input)
            .map_err(|_| crate::Failure::Transport(Error::InvalidInput))?;
        let mut invalid_progress = false;
        let outcome = self
            .invoke_with_progress(O::NAME, input, |value| {
                match serde_json::from_value(value) {
                    Ok(value) => progress(value),
                    Err(_) => invalid_progress = true,
                }
            })
            .await;
        if invalid_progress {
            return Err(crate::Failure::Transport(Error::InvalidOutput));
        }
        match outcome {
            Ok(value) => serde_json::from_value(value)
                .map_err(|_| crate::Failure::Transport(Error::InvalidOutput)),
            Err(Error::Application(value)) => Err(match serde_json::from_value(value) {
                Ok(error) => crate::Failure::Application(error),
                Err(_) => crate::Failure::Transport(Error::InvalidOutput),
            }),
            Err(error) => Err(crate::Failure::Transport(error)),
        }
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
        self.call_with_progress(command, |_| {}).await
    }

    async fn call_with_progress(
        &mut self,
        command: Command,
        mut progress: impl FnMut(Value),
    ) -> Outcome {
        let response = self.channel.exchange(command).await?;
        match response {
            Response::Events(events) => {
                let mut trace = Trace::new(self.sequence, 0);
                let mut result = None;
                let mut bearer = None;
                for event in events {
                    match trace.receive(event)? {
                        Observation::Completed(outcome) => result = Some(outcome),
                        Observation::Progress(value) => progress(value),
                        Observation::Accepted => {}
                        Observation::Bearer(change) => {
                            if bearer.is_some() {
                                return Err(Error::Protocol);
                            }
                            bearer = Some(change);
                        }
                    }
                }
                let outcome = result.ok_or(Error::Protocol)?;
                if let Some(change) = bearer {
                    if outcome.is_err() {
                        return Err(Error::Protocol);
                    }
                    self.bearer = match change {
                        crate::bearer::Change::Set(token) => Some(token),
                        crate::bearer::Change::Clear => None,
                    };
                }
                outcome
            }
            Response::Failed(error) => Err(error),
            _ => Err(Error::Protocol),
        }
    }
}

/// One invocation's channel. Hosts supply monotonic time and drive retries; ACK
/// disables only the acceptance timer. Progress is transient and completion is
/// terminal. Retransmission must retain the original invocation and logical ID.
pub struct Trace {
    id: u64,
    retry_at: u64,
    accepted: bool,
    completed: bool,
}

#[derive(Debug, PartialEq)]
pub enum Observation {
    Accepted,
    Progress(Value),
    Completed(Outcome),
    Bearer(crate::bearer::Change),
}

impl Trace {
    pub fn new(id: u64, retry_at: u64) -> Self {
        Self {
            id,
            retry_at,
            accepted: false,
            completed: false,
        }
    }

    pub fn retry_due(&self, now: u64) -> bool {
        !self.accepted && !self.completed && now >= self.retry_at
    }

    pub fn retried(&mut self, retry_at: u64) {
        self.retry_at = retry_at;
    }

    pub fn receive(&mut self, event: Event) -> Result<Observation, Error> {
        if self.completed {
            return Err(Error::Protocol);
        }
        match event {
            Event::Accepted { id } if id == self.id => {
                self.accepted = true;
                Ok(Observation::Accepted)
            }
            Event::Bearer { id, change } if id == self.id && self.accepted => {
                Ok(Observation::Bearer(change))
            }
            Event::Progress { id, value } if id == self.id && self.accepted => {
                Ok(Observation::Progress(value))
            }
            Event::Completed { id, outcome }
                if id == self.id && (self.accepted || outcome.is_err()) =>
            {
                self.completed = true;
                Ok(Observation::Completed(outcome))
            }
            _ => Err(Error::Protocol),
        }
    }
}

pub fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::InvalidOutput)
}
