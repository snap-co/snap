//! Store-backed operation declarations, composed before accepting traffic.
//! Guards inspect resident state under the dispatch gate. Handlers stage writes
//! in the caller's transaction; the platform owns loading and durable commit.
use crate::{Error, Invocation, Value};
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use snap_store::Transaction;

pub type Validator = fn(&Value) -> bool;
/// Identity and private credential are supplied by trusted platform composition.
/// A credential may support admission policy or attribution, never reauthorization
/// of accepted work in a nested persistence helper.
pub type Guard =
    fn(&mut Transaction<'_>, Option<&str>, &Value, Option<&str>) -> Result<(), snap_store::Error>;
pub type Handler = Box<
    dyn FnMut(
            &mut Transaction<'_>,
            &Invocation,
            Option<&str>,
            Option<&str>,
        ) -> Result<Value, snap_store::Error>
        + Send,
>;

pub struct Definition {
    pub name: String,
    pub identity_required: bool,
    pub input: Validator,
    pub output: Validator,
    pub progress: Validator,
    /// Evaluated once in declaration order. First failure stops acceptance.
    pub guards: &'static [Guard],
    /// Explicit Store residency, loaded by the platform before any guard runs.
    pub tables: &'static [&'static str],
    pub handler: Handler,
}

impl Definition {
    pub fn admit(
        &self,
        tx: &mut Transaction<'_>,
        actor: Option<&str>,
        input: &Value,
        bearer: Option<&str>,
    ) -> Result<(), snap_store::Error> {
        for guard in self.guards {
            guard(tx, actor, input, bearer)?;
        }
        Ok(())
    }
}

/// A stable selection, retained with queued work. Resolving a name is separate
/// from evaluating guards, and execution uses this same definition thereafter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Selection(usize);

#[derive(Default)]
pub struct Registry {
    names: BTreeMap<String, Selection>,
    definitions: Vec<Definition>,
}

impl Registry {
    pub fn register(&mut self, definition: Definition) -> Result<Selection, Error> {
        if !valid_name(&definition.name) || self.names.contains_key(&definition.name) {
            return Err(Error::Protocol);
        }
        let selection = Selection(self.definitions.len());
        self.names.insert(definition.name.clone(), selection);
        self.definitions.push(definition);
        Ok(selection)
    }
    pub fn resolve(&self, name: &str) -> Result<Selection, Error> {
        self.names.get(name).copied().ok_or(Error::UnknownOperation)
    }
    pub fn get(&self, selection: Selection) -> &Definition {
        &self.definitions[selection.0]
    }
    /// Execution may mutate handler captures, not the selected contract or guards.
    pub fn handler(&mut self, selection: Selection) -> &mut Handler {
        &mut self.definitions[selection.0].handler
    }
}

fn valid_name(name: &str) -> bool {
    name.len() <= 128
        && name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}
