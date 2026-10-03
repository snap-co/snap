//! Correlates pipelined Document submissions over transport envelopes. Unlike an
//! exchange-style channel, the carrier sends commands and receives observations
//! independently. Stable intent IDs survive replacement of this physical driver.
use crate::{ClientMessage, Completion, ServerMessage};
use alloc::collections::BTreeMap;
use snap_transport::{Command, Error, Event, Invocation, Response};

/// Topic kind for uncorrelated Document pushes.
///
/// Replication reaches every logical connection subscribed to a document, not
/// only the originator of the write that caused it, so it arrives on the global
/// path rather than on an invocation's event channel.
pub const KIND: &str = "document.replication";

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
    /// Allocate an application invocation without adding it to the Document
    /// journal. The carrier SDK must route its observations separately from
    /// `receive`; only `submit` tracks Document completions here.
    pub fn invoke(&mut self, operation: &str, input: serde_json::Value) -> Result<Command, Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        Ok(Command::Invoke(Invocation {
            id: self.sequence,
            operation: operation.into(),
            input,
        }))
    }
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

    /// Fold one frame into the journal. Returns the message it produced, if any.
    ///
    /// Transport carries exactly one event per frame, so this yields at most one
    /// message. Acceptance and progress yield none: acceptance only advances
    /// pacing, and progress goes to the supplied callback.
    pub fn receive(&mut self, response: Response) -> Result<Option<ServerMessage>, Error> {
        self.receive_with_progress(response, |_, _| {})
    }

    /// As [`Self::receive`], reporting progress against the *intent* id rather
    /// than the transport invocation id, so a caller tracking its own journal
    /// does not have to translate.
    pub fn receive_with_progress(
        &mut self,
        response: Response,
        mut progress: impl FnMut(u64, serde_json::Value),
    ) -> Result<Option<ServerMessage>, Error> {
        match response {
            // Uncorrelated server push. Delivered to every logical connection
            // subscribed to this topic, not to an originator.
            Response::Global {
                kind,
                input,
            } if kind == KIND => {
                let message =
                    serde_json::from_value(input).map_err(|_| Error::Protocol)?;
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
                Ok(Some(message))
            }
            Response::Event(event) => match event {
                Event::Accepted { id } => {
                    let pending = self.pending.get_mut(&id).ok_or(Error::Protocol)?;
                    // A repeated acceptance is a duplicate frame, not a second
                    // acknowledgement of different work.
                    if pending.accepted {
                        return Ok(None);
                    }
                    pending.accepted = true;
                    Ok(pending
                        .intent
                        .as_ref()
                        .map(|(id, _)| ServerMessage::Accepted { id: *id }))
                }
                Event::Bearer { .. } => Err(Error::Protocol),
                Event::Progress { id, value } => {
                    let pending = self.pending.get(&id).ok_or(Error::Protocol)?;
                    if !pending.accepted {
                        return Err(Error::Protocol);
                    }
                    progress(
                        pending.intent.as_ref().map_or(id, |(intent, _)| *intent),
                        value,
                    );
                    Ok(None)
                }
                Event::Completed { id, outcome } => {
                    let pending = self.pending.remove(&id).ok_or(Error::Protocol)?;
                    match outcome {
                        Ok(value) => {
                            if !pending.accepted {
                                return Err(Error::Protocol);
                            }
                            let message =
                                serde_json::from_value(value).map_err(|_| Error::Protocol)?;
                            match (&pending.intent, &message) {
                                (
                                    Some((id, document)),
                                    ServerMessage::Completed(completion),
                                ) if *id == completion.id
                                    && *document == completion.document => {}
                                (None, ServerMessage::Manifest(_)) => {}
                                _ => return Err(Error::Protocol),
                            }
                            Ok(Some(message))
                        }
                        Err(error) => {
                            // The desired mutation may already have left the
                            // journal. Still surface controller failure to the
                            // caller, without undoing that commit.
                            if matches!(&error, Error::Application(value) if value.get("committed") == Some(&serde_json::Value::Bool(true)))
                            {
                                return Err(error);
                            }
                            // An unavailable/retired carrier can hide a committed
                            // result. Retain the SDK journal for manifest recovery;
                            // do not misreport an unknown outcome as a rejection.
                            if matches!(
                                error,
                                Error::Unavailable | Error::StaleConnection | Error::InvalidBearer
                            ) {
                                return Err(error);
                            }
                            match pending.intent {
                                Some((id, document)) => Ok(Some(ServerMessage::Completed(
                                    Completion {
                                        id,
                                        document,
                                        result: Err(crate::Error::Rejected(
                                            serde_json::to_string(&error)
                                                .map_err(|_| Error::Protocol)?,
                                        )),
                                    },
                                ))),
                                None => Err(error),
                            }
                        }
                    }
                }
            },
            Response::Failed(error) => Err(error),
            _ => Err(Error::Protocol),
        }
    }
}
