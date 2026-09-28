//! Correlates pipelined Document submissions over transport envelopes. Unlike an
//! exchange-style channel, the carrier sends commands and receives observations
//! independently. Stable intent IDs survive replacement of this physical driver.
use crate::{ClientMessage, Completion, ServerMessage};
use alloc::{collections::BTreeMap, vec::Vec};
use snap_transport::{Command, Error, Event, Invocation, Response};

struct Pending {
    intent: Option<(u64, alloc::string::String)>,
    accepted: bool,
}

#[derive(Default)]
pub struct Wire {
    sequence: u64,
    pending: BTreeMap<u64, Pending>,
}

impl Wire {
    /// Forget interrupted exchanges while keeping invocation IDs unique across
    /// physical attachments. Document receipts recover stable mutation outcomes.
    pub fn reconnect(&mut self) {
        self.pending.clear();
    }
    pub fn submit(&mut self, message: ClientMessage) -> Result<Command, Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        let (operation, input, intent) = match message {
            ClientMessage::Manifest(manifest) => {
                ("document.manifest", serde_json::to_value(manifest), None)
            }
            ClientMessage::Mutate(intent) => (
                "document.mutate",
                serde_json::to_value(&intent),
                Some((intent.id, intent.document)),
            ),
        };
        let input = input.map_err(|_| Error::InvalidInput)?;
        self.pending.insert(
            self.sequence,
            Pending {
                intent,
                accepted: false,
            },
        );
        Ok(Command::Invoke(Invocation {
            id: self.sequence,
            operation: operation.into(),
            input,
        }))
    }

    pub fn receive(&mut self, response: Response) -> Result<Vec<ServerMessage>, Error> {
        self.receive_with_progress(response, |_, _| {})
    }

    pub fn receive_with_progress(
        &mut self,
        response: Response,
        mut progress: impl FnMut(u64, serde_json::Value),
    ) -> Result<Vec<ServerMessage>, Error> {
        let mut messages = Vec::new();
        match response {
            Response::Notification { operation, input } if operation == "document" => {
                let message = serde_json::from_value(input).map_err(|_| Error::Protocol)?;
                if !matches!(
                    message,
                    ServerMessage::Replication(_)
                        | ServerMessage::Committed(_)
                        | ServerMessage::Holdings(_)
                        | ServerMessage::Removed(_)
                        | ServerMessage::Reset
                ) {
                    return Err(Error::Protocol);
                }
                messages.push(message);
            }
            Response::Events(events) => {
                for event in events {
                    match event {
                        Event::Accepted { id } => {
                            let pending = self.pending.get_mut(&id).ok_or(Error::Protocol)?;
                            if pending.accepted {
                                continue;
                            }
                            pending.accepted = true;
                            if let Some((id, _)) = &pending.intent {
                                messages.push(ServerMessage::Accepted { id: *id });
                            }
                        }
                        Event::Progress { id, value } => {
                            let pending = self.pending.get(&id).ok_or(Error::Protocol)?;
                            if !pending.accepted {
                                return Err(Error::Protocol);
                            }
                            progress(
                                pending.intent.as_ref().map_or(id, |(intent, _)| *intent),
                                value,
                            );
                        }
                        Event::Completed { id, outcome } => {
                            let pending = self.pending.remove(&id).ok_or(Error::Protocol)?;
                            match outcome {
                                Ok(value) => {
                                    if !pending.accepted {
                                        return Err(Error::Protocol);
                                    }
                                    let message = serde_json::from_value(value)
                                        .map_err(|_| Error::Protocol)?;
                                    match (&pending.intent, &message) {
                                        (
                                            Some((id, document)),
                                            ServerMessage::Completed(completion),
                                        ) if *id == completion.id
                                            && *document == completion.document => {}
                                        (None, ServerMessage::Manifest(_)) => {}
                                        _ => return Err(Error::Protocol),
                                    }
                                    messages.push(message);
                                }
                                Err(error) => {
                                    // The desired mutation may already have left
                                    // the journal. Still surface controller failure
                                    // to the caller, without undoing that commit.
                                    if matches!(&error, Error::Application(value) if value.get("committed") == Some(&serde_json::Value::Bool(true)))
                                    {
                                        return Err(error);
                                    }
                                    // An unavailable/retired carrier can hide a committed
                                    // result. Retain the SDK journal for manifest recovery;
                                    // do not misreport an unknown outcome as a rejection.
                                    if matches!(
                                        error,
                                        Error::Unavailable
                                            | Error::StaleConnection
                                            | Error::InvalidBearer
                                    ) {
                                        return Err(error);
                                    }
                                    if let Some((id, document)) = pending.intent {
                                        messages.push(ServerMessage::Completed(Completion {
                                            id,
                                            document,
                                            result: Err(crate::Error::Rejected(
                                                serde_json::to_string(&error)
                                                    .map_err(|_| Error::Protocol)?,
                                            )),
                                        }));
                                    } else {
                                        return Err(error);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Response::Failed(error) => return Err(error),
            _ => return Err(Error::Protocol),
        }
        Ok(messages)
    }
}
