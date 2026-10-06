//! Authy's account SDK, composed after Identity resolves public authentication.
use alloc::{collections::BTreeMap, format, string::String, vec, vec::Vec};
use snap_store::replica::{Publication, Replica};
use snap_transport::{Channel, Error, Operation};

enum Pending {
    Subscription,
    Edit,
}

/// Portable account-profile client. This spike displays authoritative state only.
/// Reconnect reloads holdings and never resends an edit with an unknown outcome.
pub struct Profiles {
    profile: String,
    replica: Replica,
    ids: snap_transport::client::InvocationIds,
    pending: BTreeMap<u64, Pending>,
    ready: bool,
}

pub struct Update {
    pub send: Vec<snap_transport::Command>,
    pub error: Option<String>,
}

impl Profiles {
    pub fn new(profile: String) -> Result<Self, snap_store::Error> {
        snap_access::Resource::new(crate::PROFILE_KIND, &profile)?;
        Ok(Self {
            profile,
            replica: Replica::new(crate::profile_catalog())?,
            ids: Default::default(),
            pending: BTreeMap::new(),
            ready: false,
        })
    }
    pub fn profile(&mut self) -> Result<Option<crate::Profile>, snap_store::Error> {
        self.replica
            .get(crate::PROFILES, &[self.profile.clone().into()])?
            .as_ref()
            .map(crate::Profile::from_row)
            .transpose()
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn pending(&self) -> usize {
        self.pending
            .values()
            .filter(|p| matches!(p, Pending::Edit))
            .count()
    }
    pub fn sequence(&self) -> u64 {
        self.replica.sequence().unwrap_or(0)
    }
    pub fn invoke(
        &mut self,
        name: &str,
        input: snap_transport::Value,
    ) -> Result<snap_transport::Command, Error> {
        self.ids.invoke(name, input)
    }
    pub fn connect(&mut self, id: &str) -> Result<snap_transport::Command, Error> {
        if id.is_empty() {
            return Err(Error::InvalidInput);
        }
        self.ready = false;
        Ok(snap_transport::Command::Connect {
            bearer: String::new(),
            client_id: id.into(),
        })
    }
    pub fn edit(&mut self, first: &str, last: &str) -> Result<snap_transport::Command, Error> {
        if !self.ready || self.pending() != 0 {
            return Err(Error::Unavailable);
        }
        let profile = self
            .profile()
            .map_err(snap_transport::operation::storage_error)?
            .ok_or(Error::Unavailable)?;
        crate::normalize_names(first, last)
            .map_err(|e| Error::Application(serde_json::json!(e)))?;
        let input = crate::operations::EditInput {
            profile: profile.id,
            first_name: first.into(),
            last_name: last.into(),
            revision: profile.revision,
        };
        let command = self.ids.invoke(
            crate::operations::EditProfile::NAME,
            serde_json::json!(input),
        )?;
        let snap_transport::Command::Invoke(call) = &command else {
            unreachable!()
        };
        self.pending.insert(call.id, Pending::Edit);
        Ok(command)
    }
    pub fn receive(&mut self, response: snap_transport::Response) -> Result<Update, Error> {
        use snap_transport::{Event, Response};
        let mut update = Update {
            send: Vec::new(),
            error: None,
        };
        match response {
            Response::Attached { .. } => {
                if self.pending() > 0 {
                    update.error = Some("Profile save outcome unknown after reconnect; check current values before saving again".into());
                }
                self.pending.clear();
                self.replica = Replica::new(crate::profile_catalog())
                    .map_err(snap_transport::operation::storage_error)?;
                self.ready = false;
                let manifest = snap_transport::replication::Subscription {
                    tables: [(
                        crate::PROFILES.into(),
                        [vec![self.profile.clone().into()]].into_iter().collect(),
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                };
                let command = self.ids.invoke(
                    snap_transport::replication::Subscribe::NAME,
                    serde_json::json!(manifest),
                )?;
                let snap_transport::Command::Invoke(call) = &command else {
                    unreachable!()
                };
                self.pending.insert(call.id, Pending::Subscription);
                update.send.push(command);
            }
            Response::Global { kind, input } if kind == snap_transport::replication::TOPIC => {
                let publication: Publication =
                    serde_json::from_value(input).map_err(|_| Error::Protocol)?;
                self.replica
                    .apply(&publication)
                    .map_err(|_| Error::Protocol)?;
            }
            Response::Event(Event::Completed { id, outcome }) => {
                if let Some(pending) = self.pending.remove(&id) {
                    match outcome {
                        Ok(_) => {
                            if matches!(pending, Pending::Subscription) {
                                self.ready = true;
                            }
                        }
                        Err(error) => {
                            update.error = Some(format!("Profile request rejected: {error:?}"));
                        }
                    }
                }
            }
            Response::Detached => {
                self.ready = false;
                self.replica = Replica::new(crate::profile_catalog())
                    .map_err(snap_transport::operation::storage_error)?;
            }
            Response::Failed(error) => {
                self.ready = false;
                update.error = Some(format!("{error:?}"));
            }
            _ => {}
        }
        Ok(update)
    }
}
pub async fn account<C: Channel>(
    transport: &mut snap_transport::client::Client<C>,
) -> Result<crate::Account, Error> {
    let bearer = transport.bearer().map(alloc::string::String::from);
    let value = transport
        .request(
            bearer.as_deref(),
            crate::operations::FetchAccount::NAME,
            snap_transport::Value::Null,
        )
        .await?;
    snap_transport::client::decode(value)
}
