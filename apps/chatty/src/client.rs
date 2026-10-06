//! Authoritative-only messaging SDK shared by native agents and browser bindings.
//! No automatic retries or agent replies. Sequence cursors are application data,
//! distinct from physical replication positions. Reconnect starts a fresh replica.
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use snap_store::{
    Error,
    replica::{Publication, Replica},
};
use snap_transport::{Command, Event, Operation, Response};

pub struct Client {
    replica: Replica,
    ids: snap_transport::client::InvocationIds,
    selected: Option<String>,
    ready: bool,
}
pub struct Update {
    pub send: Vec<Command>,
    pub error: Option<String>,
}
impl Client {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            replica: Replica::new(crate::catalog())?,
            ids: Default::default(),
            selected: None,
            ready: false,
        })
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn connect(&mut self, bearer: String, client_id: String) -> Command {
        self.ready = false;
        Command::Connect { bearer, client_id }
    }
    pub fn invoke(
        &mut self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<Command, snap_transport::Error> {
        self.ids.invoke(name, input)
    }
    fn subscribe(&mut self) -> Result<Command, snap_transport::Error> {
        let mut collections = BTreeMap::from([(crate::THREADS.into(), serde_json::Value::Null)]);
        if let Some(thread) = &self.selected {
            collections.insert(crate::MESSAGES.into(), serde_json::json!(thread));
        }
        self.invoke(
            snap_transport::replication::Subscribe::NAME,
            serde_json::json!(snap_transport::replication::Subscription {
                collections,
                ..Default::default()
            }),
        )
    }
    pub fn select(
        &mut self,
        thread: Option<String>,
    ) -> Result<Option<Command>, snap_transport::Error> {
        self.selected = thread;
        if self.ready {
            Ok(Some(self.subscribe()?))
        } else {
            Ok(None)
        }
    }
    pub fn threads(&mut self) -> Result<Vec<crate::Thread>, Error> {
        let mut threads: Vec<_> = self
            .replica
            .rows(crate::THREADS)?
            .iter()
            .map(crate::Thread::from_row)
            .collect::<Result<_, _>>()?;
        threads.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
        Ok(threads)
    }
    pub fn messages(&mut self) -> Result<Vec<crate::Message>, Error> {
        let mut messages: Vec<_> = self
            .replica
            .rows(crate::MESSAGES)?
            .iter()
            .map(crate::Message::from_row)
            .collect::<Result<_, _>>()?;
        messages.sort_by_key(|m| m.sequence);
        Ok(messages)
    }
    pub fn receive(&mut self, response: Response) -> Result<Update, snap_transport::Error> {
        let mut update = Update {
            send: Vec::new(),
            error: None,
        };
        match response {
            Response::Attached { .. } => {
                self.replica = Replica::new(crate::catalog())
                    .map_err(snap_transport::operation::storage_error)?;
                self.ready = false;
                update.send.push(self.subscribe()?);
            }
            Response::Global { kind, input } if kind == snap_transport::replication::TOPIC => {
                let publication: Publication =
                    serde_json::from_value(input).map_err(|_| snap_transport::Error::Protocol)?;
                self.replica
                    .apply(&publication)
                    .map_err(|_| snap_transport::Error::Protocol)?;
                self.ready = true;
            }
            Response::Event(Event::Completed {
                outcome: Err(error),
                ..
            })
            | Response::Failed(error) => {
                update.error = Some(alloc::format!("{error:?}"));
            }
            Response::Detached => {
                self.ready = false;
                self.replica = Replica::new(crate::catalog())
                    .map_err(snap_transport::operation::storage_error)?;
            }
            _ => {}
        }
        Ok(update)
    }
}
