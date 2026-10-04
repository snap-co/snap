//! Dispatch and subscription policy. Persistence never authorizes an accepted
//! caller. Invocation results keep captured authority, whereas every new extent
//! or recovered subscription payload uses current Access state.
use crate::{
    Intent, Manifest, Reconciliation,
    server::{AdmittedMutation, Document},
};
use alloc::{collections::BTreeSet, string::String, vec};
use snap_access::{Access, KindDefinition, Resource, Role};
use snap_store::{Error, Transaction};

pub fn vocabulary() -> Access {
    Access::new(vec![
        KindDefinition::kind("document").expect("document kind"),
    ])
    .expect("document access vocabulary")
}

/// Portable policy evaluated only during dispatch acceptance. The serialized
/// Transport gate must remain owned until execution and commit finish.
pub struct DocumentAccessGuard<'a> {
    document: &'a Document,
}

impl<'a> DocumentAccessGuard<'a> {
    pub fn new(document: &'a Document) -> Self {
        Self { document }
    }

    pub fn require(
        tx: &mut Transaction<'_>,
        id: &str,
        actor: Option<&str>,
        minimum: Role,
    ) -> Result<(), Error> {
        let resource = Resource::new("document", id)?;
        let role = vocabulary().role(tx, &resource, actor, minimum == Role::Viewer)?;
        if snap_access::allows(role, minimum) {
            Ok(())
        } else {
            Err(Error::NotFound)
        }
    }

    pub fn admit(
        &self,
        tx: &mut Transaction<'_>,
        actor: &str,
        intent: &Intent,
    ) -> Result<Result<AdmittedMutation, crate::Error>, Error> {
        let prepared = match self.document.prepare(tx, actor, intent)? {
            Ok(prepared) => prepared,
            Err(error) => return Ok(Err(error)),
        };
        let resource = Resource::new("document", &intent.document)?;
        let role = vocabulary().role(tx, &resource, Some(actor), true)?;
        let mutation = if crate::Visibility::named(&intent.mutation).is_some() {
            None
        } else {
            Some(
                self.document
                    .registry
                    .mutation(&prepared.before, intent)
                    .map_err(|_| Error::Invalid)?,
            )
        };
        let minimum = mutation.map_or(Role::Owner, |mutation| mutation.minimum);
        if !snap_access::allows(role, minimum) {
            return Ok(Err(crate::Error::Denied));
        }
        if let Some(guard) = mutation.and_then(|mutation| mutation.guard)
            && !guard(
                tx,
                &prepared.before,
                intent,
                actor,
                role.ok_or(Error::Invalid)?,
            )?
        {
            return Ok(Err(crate::Error::Denied));
        }
        Ok(Ok(prepared))
    }

    /// Current recipient extent, separate from authority of an accepted mutation.
    pub fn extent(&self, tx: &mut Transaction<'_>, actor: &str) -> Result<BTreeSet<String>, Error> {
        let mut ids = BTreeSet::new();
        for entry in vocabulary().accessible(tx, Some(actor), true)? {
            if entry.resource.kind == "document"
                && crate::server::resource(&entry.resource.id)
                    .lifecycle(tx)?
                    .state
                    == snap_store::resource::State::Active
            {
                ids.insert(entry.resource.id);
            }
        }
        Ok(ids)
    }

    pub fn manifest(
        &self,
        tx: &mut Transaction<'_>,
        lifetime: &str,
        actor: &str,
        manifest: &Manifest,
    ) -> Result<Reconciliation, Error> {
        let extent = self.extent(tx, actor)?;
        self.document
            .manifest(tx, lifetime, actor, manifest, &extent)
    }
}
