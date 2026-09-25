use crate::{Error, Event, Invocation, Outcome, Value};
use alloc::{collections::BTreeMap, rc::Rc, string::String};
use core::cell::{Cell, RefCell};

pub type Validator = fn(&Value) -> bool;
pub struct Operation<S> {
    pub key: &'static str,
    pub identity_required: bool,
    pub input: Validator,
    pub output: Validator,
    pub error: Validator,
    /// Called after schema and identity checks, before acceptance. No handler
    /// effects belong here. This first slice uses synchronous guards/handlers.
    pub guard: fn(&Context, &S, &Value) -> Result<(), Error>,
    pub handle: fn(&Context, &mut S, Value) -> Outcome,
}

pub struct Context {
    pub identity: Option<String>,
    pub connected: bool,
}
pub trait Application: 'static {
    type State: Default + 'static;
    fn operations(&self) -> &[Operation<Self::State>];
}

/// Supplied by trusted composition. Tokens are opaque to transport. Changing a
/// token need not change identity; no session IDs, leases or cookies cross here.
pub trait Authority {
    fn identify(&self, bearer: &str) -> Option<String>;
}
impl<F: Fn(&str) -> Option<String>> Authority for F {
    fn identify(&self, bearer: &str) -> Option<String> {
        self(bearer)
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    pub reconnect_ms: u64,
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

/// Opaque, host-owned proof of one physical attachment. Old handles cannot invoke,
/// detach or close a replacement attachment. Never accepted from a wire caller.
#[derive(Clone)]
pub struct Attachment {
    key: (String, String),
    generation: u64,
}
struct Resident<S> {
    state: Rc<RefCell<S>>,
    generation: u64,
    attached: bool,
    expires: u64,
    sequence: Cell<u64>,
}

pub struct Server<A: Application, R: Authority> {
    app: A,
    authority: R,
    config: Config,
    residents: BTreeMap<(String, String), Resident<A::State>>,
    generation: u64,
    now: u64,
}
impl<A: Application, R: Authority> Server<A, R> {
    pub fn new(app: A, authority: R, config: Config) -> Result<Self, Error> {
        for (i, op) in app.operations().iter().enumerate() {
            if op.key.is_empty()
                || app.operations()[..i]
                    .iter()
                    .any(|other| other.key == op.key)
            {
                return Err(Error::Protocol);
            }
        }
        Ok(Self {
            app,
            authority,
            config,
            residents: BTreeMap::new(),
            generation: 0,
            now: 0,
        })
    }
    /// Hosts call this on their timer as well as before attachment. Time is elapsed
    /// monotonic milliseconds, not a wall-clock/session expiration timestamp.
    pub fn tick(&mut self, now: u64) {
        self.now = self.now.max(now);
        self.residents
            .retain(|_, entry| entry.attached || entry.expires > self.now);
    }
    pub fn resident_count(&self) -> usize {
        self.residents.len()
    }
    pub fn connect(
        &mut self,
        bearer: &str,
        client_id: &str,
        now: u64,
    ) -> Result<(Attachment, bool), Error> {
        self.tick(now);
        let identity = self
            .authority
            .identify(bearer)
            .filter(|id| !id.is_empty())
            .ok_or(Error::InvalidBearer)?;
        if client_id.is_empty() || client_id.len() > 128 {
            return Err(Error::InvalidInput);
        }
        let key = (identity, String::from(client_id));
        let resumed = self.residents.contains_key(&key);
        if self.residents.get(&key).is_some_and(|entry| entry.attached) {
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
                state: Rc::default(),
                generation: 0,
                attached: false,
                expires: 0,
                sequence: Cell::new(0),
            });
        entry.attached = true;
        entry.generation = self.generation;
        entry.sequence.set(0);
        Ok((
            Attachment {
                key,
                generation: self.generation,
            },
            resumed,
        ))
    }
    fn resident(&self, attachment: &Attachment) -> Result<&Resident<A::State>, Error> {
        self.residents
            .get(&attachment.key)
            .filter(|entry| entry.attached && entry.generation == attachment.generation)
            .ok_or(Error::StaleConnection)
    }
    pub fn disconnect(&mut self, attachment: &Attachment, now: u64) -> Result<(), Error> {
        self.tick(now);
        self.resident(attachment)?;
        let entry = self.residents.get_mut(&attachment.key).unwrap();
        entry.attached = false;
        entry.expires = self.now.saturating_add(self.config.reconnect_ms);
        self.tick(self.now);
        Ok(())
    }
    pub fn close(&mut self, attachment: &Attachment) -> Result<(), Error> {
        self.resident(attachment)?;
        self.residents.remove(&attachment.key);
        Ok(())
    }
    pub fn request(&self, bearer: Option<&str>, invocation: Invocation, emit: impl FnMut(Event)) {
        let identity = match bearer {
            Some(token) => match self.authority.identify(token).filter(|id| !id.is_empty()) {
                Some(id) => Some(id),
                None => {
                    let mut emit = emit;
                    emit(Event::Completed {
                        id: invocation.id,
                        outcome: Err(Error::InvalidBearer),
                    });
                    return;
                }
            },
            None => None,
        };
        self.dispatch(
            Context {
                identity,
                connected: false,
            },
            &RefCell::default(),
            invocation,
            emit,
        );
    }
    pub fn invoke(
        &self,
        attachment: &Attachment,
        invocation: Invocation,
        mut emit: impl FnMut(Event),
    ) {
        match self.resident(attachment) {
            Ok(entry) => {
                if invocation.id <= entry.sequence.get() {
                    emit(Event::Completed {
                        id: invocation.id,
                        outcome: Err(Error::Protocol),
                    });
                    return;
                }
                entry.sequence.set(invocation.id);
                self.dispatch(
                    Context {
                        identity: Some(attachment.key.0.clone()),
                        connected: true,
                    },
                    &entry.state,
                    invocation,
                    emit,
                )
            }
            Err(error) => emit(Event::Completed {
                id: invocation.id,
                outcome: Err(error),
            }),
        }
    }
    fn dispatch(
        &self,
        context: Context,
        state: &RefCell<A::State>,
        invocation: Invocation,
        mut emit: impl FnMut(Event),
    ) {
        let id = invocation.id;
        let admitted = (|| {
            let op = self
                .app
                .operations()
                .iter()
                .find(|op| op.key == invocation.operation)
                .ok_or(Error::UnknownOperation)?;
            if !(op.input)(&invocation.input) {
                return Err(Error::InvalidInput);
            }
            if op.identity_required && context.identity.is_none() {
                return Err(Error::IdentityRequired);
            }
            if let Err(error) = (op.guard)(&context, &state.borrow(), &invocation.input) {
                if let Error::Application(value) = &error
                    && !(op.error)(value)
                {
                    return Err(Error::InvalidOutput);
                }
                return Err(error);
            }
            Ok(op)
        })();
        let outcome = match admitted {
            Err(error) => Err(error),
            Ok(op) => {
                // Emit before even entering synchronous application code. Failed
                // delivery must be recorded by the adapter, not cancel this call.
                emit(Event::Accepted { id });
                let outcome = (op.handle)(&context, &mut state.borrow_mut(), invocation.input);
                match &outcome {
                    Ok(value) if !(op.output)(value) => Err(Error::InvalidOutput),
                    Err(Error::Application(value)) if !(op.error)(value) => {
                        Err(Error::InvalidOutput)
                    }
                    _ => outcome,
                }
            }
        };
        emit(Event::Completed { id, outcome });
    }
}
