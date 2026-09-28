use crate::{Error, Intent, Snapshot};
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use serde_json::Value;
use sha2::{Digest, Sha256};
use snap_access::Role;

pub type Apply = fn(&Value, &Value, &str) -> Result<Value, Error>;
/// Read related resident state during protected admission. Guards must not stage
/// writes; the dispatcher's read-only transaction enforces this before ACK.
/// Store misses remain storage errors rather than becoming authorization denials.
pub type Guard = fn(
    &mut snap_store::Transaction<'_>,
    &Snapshot,
    &Intent,
    &str,
    Role,
) -> Result<bool, snap_store::Error>;

pub struct Mutation {
    pub name: String,
    pub minimum: Role,
    pub apply: Apply,
    pub guard: Option<Guard>,
}

pub struct Definition {
    pub kind: String,
    pub version: String,
    pub validate: fn(&Value) -> bool,
    pub mutations: Vec<Mutation>,
}

pub struct Registry {
    definitions: BTreeMap<String, Definition>,
}

impl Registry {
    pub fn new(definitions: Vec<Definition>) -> Result<Self, Error> {
        let mut registered = BTreeMap::new();
        for definition in definitions {
            if definition.kind.is_empty()
                || definition.version.is_empty()
                || registered.contains_key(&definition.kind)
            {
                return Err(Error::Invalid);
            }
            let mut names = alloc::collections::BTreeSet::new();
            for mutation in &definition.mutations {
                if mutation.name.is_empty()
                    || mutation.name.starts_with("document.")
                    || !names.insert(mutation.name.clone())
                {
                    return Err(Error::Invalid);
                }
            }
            registered.insert(definition.kind.clone(), definition);
        }
        Ok(Self {
            definitions: registered,
        })
    }

    pub fn definition(&self, kind: &str) -> Result<&Definition, Error> {
        self.definitions.get(kind).ok_or(Error::Incompatible)
    }

    pub fn validate(&self, snapshot: &Snapshot) -> Result<(), Error> {
        let definition = self.definition(&snapshot.kind)?;
        if definition.version != snapshot.version {
            return Err(Error::Incompatible);
        }
        if !(definition.validate)(&snapshot.value) || snapshot.revision == 0 {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn mutation(&self, snapshot: &Snapshot, intent: &Intent) -> Result<&Mutation, Error> {
        self.validate(snapshot)?;
        if intent.document != snapshot.id || intent.version != snapshot.version || intent.id == 0 {
            return Err(Error::Incompatible);
        }
        self.definition(&snapshot.kind)?
            .mutations
            .iter()
            .find(|mutation| mutation.name == intent.mutation)
            .ok_or(Error::Invalid)
    }

    /// Pure behavior shared by authority, optimistic replay and remote replication.
    /// Authorization is checked separately by the server against current Access state.
    pub fn apply(
        &self,
        snapshot: &Snapshot,
        intent: &Intent,
        actor: &str,
    ) -> Result<Snapshot, Error> {
        if crate::lifecycle::Operation::named(&intent.mutation).is_some() {
            self.validate(snapshot)?;
            if intent.id == 0
                || intent.document != snapshot.id
                || intent.version != snapshot.version
                || !intent.args.is_null()
            {
                return Err(Error::Invalid);
            }
            // Cleanup and blocked status belong to the authority. Keep the visible
            // value until its committed holdings/removal arrives, without inventing
            // a value revision for a lifecycle-only mutation.
            return Ok(snapshot.clone());
        }
        let mutation = self.mutation(snapshot, intent)?;
        let mut result = snapshot.clone();
        result.value = (mutation.apply)(&snapshot.value, &intent.args, actor)?;
        result.revision = result.revision.checked_add(1).ok_or(Error::Invalid)?;
        self.validate(&result)?;
        Ok(result)
    }
}

/// Object keys are recursively sorted, independently of serde_json feature flags.
/// Includes kind, version and revision, so matching payloads at different bases
/// cannot silently pass the intent-replication precondition.
pub fn digest(snapshot: &Snapshot) -> String {
    let value = serde_json::to_value(snapshot).expect("document JSON is serializable");
    let mut canonical = String::new();
    canonical_json(&value, &mut canonical);
    let bytes = Sha256::digest(canonical.as_bytes());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_json(value: &Value, output: &mut String) {
    match value {
        Value::Object(fields) => {
            output.push('{');
            let ordered: BTreeMap<_, _> = fields.iter().collect();
            for (index, (key, value)) in ordered.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).expect("JSON key"));
                output.push(':');
                canonical_json(value, output);
            }
            output.push('}');
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                canonical_json(value, output);
            }
            output.push(']');
        }
        _ => output.push_str(&serde_json::to_string(value).expect("JSON scalar")),
    }
}
