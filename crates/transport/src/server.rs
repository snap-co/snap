//! Trusted connection context and logical lifetime, independent of application
//! execution. A platform selects its dispatcher and owns resident application data.
use crate::{Error, Invocation};
use alloc::{collections::BTreeMap, string::String, vec::Vec};

/// Supplied by trusted composition. Tokens are opaque to transport. Changing a
/// token need not change identity; no session IDs, leases or cookies cross here.
pub trait Authority {
    fn identify(&self, bearer: &str) -> Result<String, Error>;
    /// Lifetime eligibility only, never permission to connect or invoke. Hosts
    /// with renewable access may retain state while access is expired or rotation
    /// is owned. Absolute credential expiry and known revocation still retire it.
    fn retained(&self, bearer: &str) -> Result<String, Error> {
        self.identify(bearer)
    }
}
impl<F: Fn(&str) -> Option<String>> Authority for F {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        self(bearer).ok_or(Error::InvalidBearer)
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    /// Detached retention, in monotonic milliseconds. Defaults to five minutes.
    pub reconnect_ms: u64,
    /// Attached and detached logical connections both count toward capacity.
    pub capacity: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            reconnect_ms: 300_000,
            capacity: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Open,
    Draining,
    Closed,
}

/// Opaque, host-owned proof of one physical attachment. Never accepted from a
/// wire caller. Old handles cannot invoke, detach or close a replacement.
#[derive(Clone)]
pub struct Attachment {
    key: (String, String),
    generation: u64,
    connection: ConnectionId,
}
impl Attachment {
    pub fn connection(&self) -> ConnectionId {
        self.connection
    }
}
struct Resident {
    bearer: String,
    id: ConnectionId,
    generation: u64,
    attached: bool,
    expires: u64,
    sequence: u64,
    closing: bool,
    accepted: usize,
}

/// Verified input for the host dispatcher. The wire has no asserted identity,
/// connection ID or server attachment. Request calls have no resident scope.
pub struct Dispatch {
    pub identity: Option<String>,
    pub connection: Option<ConnectionId>,
    pub invocation: Invocation,
}

pub struct Server<R: Authority> {
    authority: R,
    config: Config,
    residents: BTreeMap<(String, String), Resident>,
    retired: Vec<ConnectionId>,
    generation: u64,
    now: u64,
    live_authority: bool,
}
impl<R: Authority> Server<R> {
    pub fn new(authority: R, config: Config) -> Self {
        Self {
            authority,
            config,
            residents: BTreeMap::new(),
            retired: Vec::new(),
            generation: 0,
            now: 0,
            live_authority: false,
        }
    }
    /// Revalidate opaque credentials on every connected invocation and timer tick.
    /// Any failed validation retires the connection, including unavailable storage.
    /// This closes a physical lifetime, not the persisted login session. Hosts tick.
    pub fn with_live_authority(mut self) -> Self {
        self.live_authority = true;
        self
    }
    pub fn attached(&self, attachment: &Attachment) -> bool {
        self.residents.get(&attachment.key).is_some_and(|entry| {
            !entry.closing && entry.attached && entry.generation == attachment.generation
        })
    }
    /// Hosts drive this even without traffic, then drain `take_retired`. Retiring
    /// a connection revokes new dispatch; already owned work may finish before
    /// the execution host releases its associated resident data.
    pub fn tick(&mut self, now: u64) {
        self.now = self.now.max(now);
        self.residents.retain(|(identity, _), entry| {
            let valid = entry.accepted > 0
                || !self.live_authority
                || match self.authority.retained(&entry.bearer) {
                    Ok(current) => current == *identity,
                    Err(_) => false,
                };
            if !valid || (!entry.attached && entry.expires <= self.now) {
                entry.closing = true;
                entry.attached = false;
            }
            let keep = !entry.closing || entry.accepted > 0;
            if !keep {
                self.retired.push(entry.id);
            }
            keep
        });
    }
    pub fn take_retired(&mut self) -> Vec<ConnectionId> {
        core::mem::take(&mut self.retired)
    }
    pub fn resident_count(&self) -> usize {
        self.residents.len()
    }
    /// Resolve the bearer for every attachment. The returned flag is true only
    /// when reconnecting to retained logical state. An occupied owner is untouched.
    pub fn connect(
        &mut self,
        bearer: &str,
        client_id: &str,
        now: u64,
    ) -> Result<(Attachment, bool), Error> {
        self.tick(now);
        let identity = self.authority.identify(bearer)?;
        if identity.is_empty() {
            return Err(Error::InvalidBearer);
        }
        if client_id.is_empty() || client_id.len() > 128 {
            return Err(Error::InvalidInput);
        }
        let key = (identity, String::from(client_id));
        let resumed = self.residents.contains_key(&key);
        if self
            .residents
            .get(&key)
            .is_some_and(|entry| entry.attached || entry.closing)
        {
            return Err(Error::Occupied);
        }
        if !resumed && self.residents.len() >= self.config.capacity {
            return Err(Error::Capacity);
        }
        self.generation = self.generation.checked_add(1).ok_or(Error::Capacity)?;
        let entry = self
            .residents
            .entry(key.clone())
            .or_insert_with(|| Resident {
                bearer: String::from(bearer),
                id: ConnectionId(self.generation),
                generation: 0,
                attached: false,
                expires: 0,
                sequence: 0,
                closing: false,
                accepted: 0,
            });
        entry.attached = true;
        entry.bearer = String::from(bearer);
        entry.generation = self.generation;
        entry.sequence = 0;
        Ok((
            Attachment {
                key,
                generation: self.generation,
                connection: entry.id,
            },
            resumed,
        ))
    }
    fn resident(&mut self, attachment: &Attachment) -> Result<&mut Resident, Error> {
        self.residents
            .get_mut(&attachment.key)
            .filter(|entry| {
                !entry.closing && entry.attached && entry.generation == attachment.generation
            })
            .ok_or(Error::StaleConnection)
    }
    /// Detach without retiring resident state until the reconnect deadline.
    pub fn disconnect(&mut self, attachment: &Attachment, now: u64) -> Result<(), Error> {
        self.tick(now);
        let expires = self.now.saturating_add(self.config.reconnect_ms);
        let entry = self.resident(attachment)?;
        entry.attached = false;
        entry.expires = expires;
        self.tick(self.now);
        Ok(())
    }
    /// Stop admissions immediately. Accepted operations retain the logical scope
    /// until their final release, independently of the physical attachment.
    pub fn close(&mut self, attachment: &Attachment) -> Result<(), Error> {
        let entry = self.resident(attachment)?;
        entry.closing = true;
        entry.attached = false;
        self.tick(self.now);
        Ok(())
    }

    pub fn state(&self, connection: ConnectionId) -> ConnectionState {
        match self.residents.values().find(|entry| entry.id == connection) {
            Some(entry) if entry.closing => ConnectionState::Draining,
            Some(_) => ConnectionState::Open,
            None => ConnectionState::Closed,
        }
    }

    /// Called by the dispatcher under its gate after guards pass, before ACK.
    pub fn retain(&mut self, connection: ConnectionId) -> Result<(), Error> {
        let entry = self
            .residents
            .values_mut()
            .find(|entry| entry.id == connection && !entry.closing)
            .ok_or(Error::StaleConnection)?;
        entry.accepted = entry.accepted.checked_add(1).ok_or(Error::Capacity)?;
        Ok(())
    }

    /// Release exactly one accepted operation after execution and controllers.
    pub fn release(&mut self, connection: ConnectionId) -> Result<(), Error> {
        let entry = self
            .residents
            .values_mut()
            .find(|entry| entry.id == connection)
            .ok_or(Error::StaleConnection)?;
        entry.accepted = entry.accepted.checked_sub(1).ok_or(Error::Protocol)?;
        self.tick(self.now);
        Ok(())
    }
    /// Non-connection requests resolve credentials on every invocation. Their
    /// execution state is temporary and must not become a resident connection.
    pub fn request(&self, bearer: Option<&str>, invocation: Invocation) -> Result<Dispatch, Error> {
        let identity = bearer
            .map(|token| {
                let identity = self.authority.identify(token)?;
                if identity.is_empty() {
                    Err(Error::InvalidBearer)
                } else {
                    Ok(identity)
                }
            })
            .transpose()?;
        Ok(Dispatch {
            identity,
            connection: None,
            invocation,
        })
    }
    pub fn invoke(
        &mut self,
        attachment: &Attachment,
        invocation: Invocation,
    ) -> Result<Dispatch, Error> {
        if self.live_authority {
            let bearer = self.resident(attachment)?.bearer.clone();
            let validation = match self.authority.identify(&bearer) {
                Ok(identity) if identity == attachment.key.0 => Ok(()),
                Ok(_) => Err(Error::InvalidBearer),
                Err(error) => Err(error),
            };
            if let Err(error) = validation {
                self.close(attachment)?;
                return Err(error);
            }
        }
        let entry = self.resident(attachment)?;
        if invocation.id <= entry.sequence {
            return Err(Error::Protocol);
        }
        entry.sequence = invocation.id;
        Ok(Dispatch {
            identity: Some(attachment.key.0.clone()),
            connection: Some(entry.id),
            invocation,
        })
    }
}
