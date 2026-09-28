//! Store-backed document server: guarded mutations, receipts and replacement.
//!
//! Portable `no_std` with `alloc`; hosts own execution, lifetimes and external
//! IO. All methods take the caller's `&mut Transaction` so Access and
//! caller-owned rows commit atomically. Publication happens only after the
//! caller observes `Committed`; this crate performs no live IO.
//!
//! Non-obvious guarantees promised beside this interface:
//!
//! * One caller-owned transaction composes Access and Document writes. Failed
//!   transactions publish nothing; staged writes are discarded by Store.
//! * Document resources use Access kind `"document"` (the Access vocabulary
//!   must contain it). `Snapshot::kind` selects the app `Definition` in the
//!   `Registry`; the two kinds are independent.
//! * `create` registers the Access resource (owner becomes `Owner`) and inserts
//!   the document row atomically. Duplicates, kind/audience conflicts and
//!   registry failures report `StoreError::Invalid`; cold tables report `Miss`.
//! * `mutate` serializes onto the latest stored revision. There is no blanket
//!   stale-base rejection: the intent carries no base revision and always
//!   applies to what is currently committed.
//! * Receipts are keyed by `(lifetime, intent.id)` and bind the exact actor and
//!   intent (`document`, `version`, `mutation`, `args`). The same key with the
//!   same actor and byte-equal intent replays the stored completion without
//!   re-executing (`replayed: true`, `replication: None`). The same key with a
//!   different actor or intent is misuse (`StoreError::Invalid`) and stages
//!   nothing new.
//! * Successful writes stage the document update and the receipt insert
//!   together; both commit atomically. Denials and declared rejections stage
//!   only the receipt (a `Completion` with `Err`), so retries dedup without
//!   partial document writes. Guard and minimum-role checks run against the
//!   pre-change Access state.
//! * A reread (retry or `manifest` recovery) suppresses the payload when the
//!   actor can no longer read the document: `Ok(Some(_))` becomes `Ok(None)`.
//!   `Ok(None)` means "committed but forbidden"; it never carries a snapshot.
//! * `manifest` reconciles all authorized active Documents: missing/stale
//!   snapshots, validated unchanged holdings, and recovered pending completions.
//!   Deleted/archived Documents are retained for cleanup but excluded from loading.
//! * `expire` deletes receipt rows for one lifetime only; document rows are
//!   never removed here.
//! * Any Store `Miss` aborts the attempt and is never converted into a domain
//!   `Completion`. Handlers must propagate it with `?`.
//! * Misuse (empty lifetime/actor, zero or storage-unsafe IDs, malformed UUIDs,
//!   conflicting receipt reuse) reports `StoreError::Invalid`. Domain failures
//!   (denied, missing, incompatible, rejected) report inside `Completion`.
//! * Revisions and intent IDs must fit in signed 64-bit storage
//!   (`1..=i64::MAX`). Larger values report `StoreError::Invalid`.
//! * The host namespaces `lifetime` per boot (for example `"boot:connection"`),
//!   authenticates and admits under its execution gate before ACK. Accepted
//!   invocation results retain that authority; ongoing synchronization uses the
//!   current authorized manifest. Transport
//!   invocation IDs are unrelated to stable `Intent::id` values.

#![allow(clippy::too_many_lines)]

extern crate alloc;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    vec::Vec,
};
use snap_access::{Access, Audience, DirectGrant, Resource, Role};
use snap_store::{Row, Transaction, Value};

use crate::{Completion, Intent, Manifest, Reconciliation, Replication, Snapshot, digest};
use crate::{Error as DomainError, Registry};
use snap_store::Error as StoreError;

/// Ordered migration declarations for the Document tables.
pub const MIGRATION: &str = include_str!("../migrations/0001_document.toml");
pub const LIFECYCLE_MIGRATION: &str = include_str!("../migrations/0005_document_lifecycle.toml");
/// Store tables owned by this module. Hosts load these to arrange residency.
pub const TABLES: [&str; 3] = [
    "document.documents",
    "document.receipts",
    "document.lifecycle",
];

const DOCUMENTS: &str = TABLES[0];
const RECEIPTS: &str = TABLES[1];
const LIFECYCLE: &str = TABLES[2];
/// Access kind for every document resource. App definitions select behavior
/// through `Snapshot::kind` instead.
const RESOURCE_KIND: &str = "document";

/// Successful or recovered mutation outcome.
///
/// `replayed` is true when the completion came from an existing receipt
/// without re-executing. First executions report `false`. Replays never carry
/// `replication` so hosts cannot fan the same write out twice; only committed
/// results reach publication, and only after the caller's `Committed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationResult {
    pub completion: Completion,
    pub replication: Option<Replication>,
    pub replayed: bool,
}

/// Admission captures the pre-change authority and snapshot. The host must retain
/// its application gate until execution finishes; no other writer may intervene.
pub struct AdmittedMutation {
    before: Snapshot,
    intent: Intent,
    actor: String,
}

/// Document server over the caller's Store transaction.
///
/// Holds the app `Registry` (deterministic behavior) and the `Access`
/// vocabulary (authorization). Rows live in the transaction; this struct holds
/// no per-document state.
pub struct Document {
    /// App definitions selecting deterministic behavior by `Snapshot::kind`.
    pub registry: Registry,
    /// Authorization vocabulary; must contain kind `"document"`.
    pub access: Access,
}

impl Document {
    /// Authorized identities for the host residency controller. This queries the
    /// resident Access graph, not Document contents or storage IO.
    pub fn authorized_ids(
        &self,
        tx: &mut Transaction<'_>,
        actor: &str,
    ) -> Result<BTreeSet<String>, StoreError> {
        let mut ids = BTreeSet::new();
        for entry in self.access.accessible(tx, Some(actor), true)? {
            if entry.resource.kind == RESOURCE_KIND
                && self.lifecycle(tx, &entry.resource.id)?.state == crate::lifecycle::State::Active
            {
                ids.insert(entry.resource.id);
            }
        }
        Ok(ids)
    }
    /// Translate a committed Store change to current Document state. Deletion
    /// requires retained lifecycle state so controllers can finish cleanup.
    pub fn changed(
        &self,
        tx: &mut Transaction<'_>,
        change: &snap_store::RowChange,
    ) -> Result<Option<Snapshot>, StoreError> {
        if change.table == LIFECYCLE {
            return tx
                .get(DOCUMENTS, &change.key)?
                .as_ref()
                .map(snapshot_from_row)
                .transpose();
        }
        if change.table != DOCUMENTS {
            return Ok(None);
        }
        change.after.as_ref().map(snapshot_from_row).transpose()
    }

    /// Host-only residency/recovery scan. This grants no client read authority.
    pub fn snapshots(&self, tx: &mut Transaction<'_>) -> Result<Vec<Snapshot>, StoreError> {
        tx.find(DOCUMENTS, "primary", &[])?
            .iter()
            .map(snapshot_from_row)
            .collect()
    }

    /// Cleanup ownership retains these rows independently of connected viewers.
    /// Lifecycle metadata is resident even when Document values are partially loaded.
    pub fn cleanup_ids(&self, tx: &mut Transaction<'_>) -> Result<BTreeSet<String>, StoreError> {
        let mut ids = BTreeSet::new();
        for row in tx.find(LIFECYCLE, "primary", &[])? {
            let Some(Value::Text(id)) = row.get("id") else {
                return Err(StoreError::Invalid);
            };
            if !self.lifecycle(tx, id)?.finalizers.is_empty() {
                ids.insert(id.clone());
            }
        }
        Ok(ids)
    }
    /// Assemble the server from an app registry and an Access vocabulary.
    ///
    /// The supplied `Access` must already contain kind `"document"`
    /// (for example `KindDefinition::kind("document")`); otherwise every
    /// operation touching Access reports `StoreError::Invalid`.
    pub fn new(registry: Registry, access: Access) -> Self {
        Self { registry, access }
    }

    /// Atomically register the Access resource and insert the document row.
    ///
    /// The owner becomes `Owner` on the new `"document"` resource. Rejects
    /// duplicates (same id already stored), conflicting Access state, unknown
    /// app kinds/versions, invalid values, zero or storage-unsafe revisions,
    /// malformed ids and empty owners with `StoreError::Invalid`. Cold tables
    /// report `Miss` and stage nothing.
    pub fn create(
        &self,
        tx: &mut Transaction<'_>,
        snapshot: &Snapshot,
        audience: Audience,
        owner: &str,
    ) -> Result<(), StoreError> {
        if owner.is_empty() {
            return Err(StoreError::Invalid);
        }
        if !is_uuid(&snapshot.id) {
            return Err(StoreError::Invalid);
        }
        if snapshot.revision == 0 || snapshot.revision > i64::MAX as u64 {
            return Err(StoreError::Invalid);
        }
        self.registry
            .validate(snapshot)
            .map_err(|_| StoreError::Invalid)?;
        // Access registration rejects existing resource identities. A new complete
        // insert need not load an arbitrary UUID merely to establish absence;
        // Store/database constraints still reject a conflicting Document row.
        let resource =
            Resource::new(RESOURCE_KIND, &snapshot.id).map_err(|_| StoreError::Invalid)?;
        let grant = DirectGrant::new(owner, Role::Owner).map_err(|_| StoreError::Invalid)?;
        let created = self
            .access
            .register(tx, &resource, audience, &[grant], None)?;
        if !created {
            return Err(StoreError::Invalid);
        }
        tx.insert(DOCUMENTS, snapshot_row(snapshot)?)?;
        Ok(())
    }

    /// Authorized read of one document.
    ///
    /// Requires at least `Viewer` with audience-derived viewing. Denials
    /// report `StoreError::Invalid` without leaking the value; missing rows
    /// report `StoreError::NotFound`; cold reads report `Miss`.
    pub fn read(
        &self,
        tx: &mut Transaction<'_>,
        id: &str,
        actor: Option<&str>,
    ) -> Result<Snapshot, StoreError> {
        if !is_uuid(id) {
            return Err(StoreError::Invalid);
        }
        if let Some(who) = actor
            && who.is_empty()
        {
            return Err(StoreError::Invalid);
        }
        let row = tx
            .get(DOCUMENTS, &[Value::Text(id.into())])?
            .ok_or(StoreError::NotFound)?;
        let snapshot = snapshot_from_row(&row)?;
        self.registry
            .validate(&snapshot)
            .map_err(|_| StoreError::Invalid)?;
        let resource = Resource::new(RESOURCE_KIND, id).map_err(|_| StoreError::Invalid)?;
        let role = self.access.role(tx, &resource, actor, true)?;
        if !snap_access::allows(role, Role::Viewer) {
            return Err(StoreError::Invalid);
        }
        Ok(snapshot)
    }

    /// Trusted controller observation, including retained deleted/archived values.
    /// This grants no client authority; wire operations must use `read` instead.
    pub fn retained(&self, tx: &mut Transaction<'_>, id: &str) -> Result<Snapshot, StoreError> {
        if !is_uuid(id) {
            return Err(StoreError::Invalid);
        }
        let row = tx
            .get(DOCUMENTS, &[Value::Text(id.into())])?
            .ok_or(StoreError::NotFound)?;
        let snapshot = snapshot_from_row(&row)?;
        self.registry
            .validate(&snapshot)
            .map_err(|_| StoreError::Invalid)?;
        Ok(snapshot)
    }

    /// Replace a document from a trusted application operation, for example a
    /// committed external-model result. Requires pre-change Owner authority and
    /// validates the existing kind/version. This is not a wire operation: callers
    /// must apply their session and domain guards in the same transaction. It has
    /// no optimistic intent or receipt; publish authoritative holdings after commit.
    pub fn replace(
        &self,
        tx: &mut Transaction<'_>,
        id: &str,
        actor: &str,
        value: serde_json::Value,
    ) -> Result<Snapshot, StoreError> {
        let mut snapshot = self.read(tx, id, Some(actor))?;
        let resource = Resource::new(RESOURCE_KIND, id).map_err(|_| StoreError::Invalid)?;
        if !snap_access::allows(
            self.access.role(tx, &resource, Some(actor), false)?,
            Role::Owner,
        ) {
            return Err(StoreError::Invalid);
        }
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(StoreError::Invalid)?;
        snapshot.value = value;
        self.registry
            .validate(&snapshot)
            .map_err(|_| StoreError::Invalid)?;
        tx.update(
            DOCUMENTS,
            &[id.into()],
            [
                ("revision".into(), (snapshot.revision as i64).into()),
                ("value".into(), snapshot.value.to_string().into()),
            ]
            .into_iter()
            .collect(),
        )?;
        Ok(snapshot)
    }

    /// Request deletion under Owner authority. Retain data, Access and finalizers
    /// for cleanup, while excluding the Document from normal synchronization.
    pub fn remove(
        &self,
        tx: &mut Transaction<'_>,
        id: &str,
        actor: &str,
    ) -> Result<(), StoreError> {
        self.read(tx, id, Some(actor))?;
        let resource = Resource::new(RESOURCE_KIND, id).map_err(|_| StoreError::Invalid)?;
        if !snap_access::allows(
            self.access.role(tx, &resource, Some(actor), false)?,
            Role::Owner,
        ) {
            return Err(StoreError::Invalid);
        }
        let mut lifecycle = self.lifecycle(tx, id)?;
        lifecycle.state = crate::lifecycle::State::Deleted;
        self.set_lifecycle(tx, id, &lifecycle)?;
        Ok(())
    }

    /// Trusted controller publication, independent of a currently connected owner.
    /// Values are schema checked; unchanged observations do not enqueue a new pass.
    /// Never expose this method as an unguarded wire replacement operation.
    pub fn observe(
        &self,
        tx: &mut Transaction<'_>,
        id: &str,
        value: serde_json::Value,
    ) -> Result<Snapshot, StoreError> {
        let mut snapshot = self.retained(tx, id)?;
        if snapshot.value == value {
            return Ok(snapshot);
        }
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(StoreError::Invalid)?;
        snapshot.value = value;
        self.registry
            .validate(&snapshot)
            .map_err(|_| StoreError::Invalid)?;
        tx.update(
            DOCUMENTS,
            &[id.into()],
            [
                ("revision".into(), (snapshot.revision as i64).into()),
                ("value".into(), snapshot.value.to_string().into()),
            ]
            .into_iter()
            .collect(),
        )?;
        Ok(snapshot)
    }

    pub fn lifecycle(
        &self,
        tx: &mut Transaction<'_>,
        id: &str,
    ) -> Result<crate::lifecycle::Lifecycle, StoreError> {
        let Some(row) = tx.get(LIFECYCLE, &[id.into()])? else {
            return Ok(Default::default());
        };
        let text = |name: &str| match row.get(name) {
            Some(Value::Text(value)) => Ok(value.as_str()),
            _ => Err(StoreError::Invalid),
        };
        Ok(crate::lifecycle::Lifecycle {
            state: serde_json::from_str(text("state")?).map_err(|_| StoreError::Invalid)?,
            finalizers: serde_json::from_str(text("finalizers")?)
                .map_err(|_| StoreError::Invalid)?,
            blocked: serde_json::from_str(text("blocked")?).map_err(|_| StoreError::Invalid)?,
        })
    }

    /// Platform/controller-owned metadata. Public operations must check Access
    /// before calling this, just as for their composed domain mutations.
    pub fn set_lifecycle(
        &self,
        tx: &mut Transaction<'_>,
        id: &str,
        lifecycle: &crate::lifecycle::Lifecycle,
    ) -> Result<(), StoreError> {
        if self.lifecycle(tx, id)? == *lifecycle {
            return Ok(());
        }
        let fields: Row = [
            (
                "state".into(),
                serde_json::to_string(&lifecycle.state)
                    .map_err(|_| StoreError::Invalid)?
                    .into(),
            ),
            (
                "finalizers".into(),
                serde_json::to_string(&lifecycle.finalizers)
                    .map_err(|_| StoreError::Invalid)?
                    .into(),
            ),
            (
                "blocked".into(),
                serde_json::to_string(&lifecycle.blocked)
                    .map_err(|_| StoreError::Invalid)?
                    .into(),
            ),
        ]
        .into_iter()
        .collect();
        if tx.get(LIFECYCLE, &[id.into()])?.is_some() {
            tx.update(LIFECYCLE, &[id.into()], fields)
        } else {
            let mut row = fields;
            row.insert("id".into(), id.into());
            tx.insert(LIFECYCLE, row)
        }
    }

    /// Execute one named mutation on one document inside the caller's
    /// transaction.
    ///
    /// See the module guarantees for receipt binding, pre-change
    /// authorization, latest-state serialization, suppression and error
    /// mapping. The returned `replication` (when `Some`) is for fan-out to
    /// other holders after `Committed`; the originator keeps `completion`.
    pub fn mutate(
        &self,
        tx: &mut Transaction<'_>,
        lifetime: &str,
        actor: &str,
        intent: &Intent,
    ) -> Result<MutationResult, StoreError> {
        validate_lifetime(lifetime)?;
        validate_actor(actor)?;
        validate_intent_shape(intent)?;
        let intent_key = intent.id as i64;

        // Receipt reread first: exact replays suppress when revoked, conflicts
        // report Invalid, misses abort.
        if let Some(row) = tx.get(
            RECEIPTS,
            &[Value::Text(lifetime.into()), Value::Integer(intent_key)],
        )? {
            let stored = receipt_from_row(&row)?;
            if stored.actor != actor
                || stored.document != intent.document
                || stored.version != intent.version
                || stored.mutation != intent.mutation
                || stored.args != intent.args
            {
                return Err(StoreError::Invalid);
            }
            let completion = self.suppress_if_revoked(tx, actor, stored.completion)?;
            return Ok(MutationResult {
                completion,
                replication: None,
                replayed: true,
            });
        }

        let admitted = self.admit(tx, actor, intent)?;
        let result = match admitted {
            Ok(admitted) => self.execute(tx, admitted)?,
            Err(error) => MutationResult {
                completion: Completion {
                    id: intent.id,
                    document: intent.document.clone(),
                    result: Err(error),
                },
                replication: None,
                replayed: false,
            },
        };
        insert_receipt(tx, lifetime, intent_key, actor, intent, &result.completion)?;
        Ok(result)
    }

    /// Check Access and mutation guards before ACK. This does not execute the
    /// mutation or stage writes. Storage failures remain distinct from rejection.
    pub fn admit(
        &self,
        tx: &mut Transaction<'_>,
        actor: &str,
        intent: &Intent,
    ) -> Result<Result<AdmittedMutation, DomainError>, StoreError> {
        validate_actor(actor)?;
        validate_intent_shape(intent)?;
        let current = tx.get(DOCUMENTS, &[Value::Text(intent.document.clone())])?;
        let Some(row) = current else {
            return Ok(Err(DomainError::NotFound));
        };
        let before = snapshot_from_row(&row).map_err(|_| StoreError::Invalid)?;

        if matches!(
            intent.mutation.as_str(),
            "document.delete" | "document.archive" | "document.retry"
        ) {
            if intent.version != before.version || !intent.args.is_null() {
                return Ok(Err(DomainError::Invalid));
            }
            let resource =
                Resource::new(RESOURCE_KIND, &intent.document).map_err(|_| StoreError::Invalid)?;
            if !snap_access::allows(
                self.access.role(tx, &resource, Some(actor), false)?,
                Role::Owner,
            ) {
                return Ok(Err(DomainError::Denied));
            }
            return Ok(Ok(AdmittedMutation {
                before,
                intent: intent.clone(),
                actor: actor.into(),
            }));
        }
        if self.lifecycle(tx, &intent.document)?.state != crate::lifecycle::State::Active {
            return Ok(Err(DomainError::NotFound));
        }

        let mutation = match self.registry.mutation(&before, intent) {
            Ok(found) => found,
            Err(error) => {
                return Ok(Err(error));
            }
        };

        // Pre-change authorization: minimum role plus optional guard.
        let resource =
            Resource::new(RESOURCE_KIND, &intent.document).map_err(|_| StoreError::Invalid)?;
        let role = self.access.role(tx, &resource, Some(actor), true)?;
        if !snap_access::allows(role, mutation.minimum) {
            return Ok(Err(DomainError::Denied));
        }
        if let Some(guard) = mutation.guard {
            let effective = role.ok_or(StoreError::Invalid)?;
            if !(guard)(tx, &before, intent, actor, effective)? {
                return Ok(Err(DomainError::Denied));
            }
        }
        Ok(Ok(AdmittedMutation {
            before,
            intent: intent.clone(),
            actor: actor.into(),
        }))
    }

    /// Compose a named mutation in the caller's transaction without a wire receipt.
    /// Callers must propagate a rejected completion to abort composite operations.
    pub fn apply(
        &self,
        tx: &mut Transaction<'_>,
        actor: &str,
        intent: &Intent,
    ) -> Result<MutationResult, StoreError> {
        match self.admit(tx, actor, intent)? {
            Ok(admitted) => self.execute(tx, admitted),
            Err(error) => Ok(MutationResult {
                completion: Completion {
                    id: intent.id,
                    document: intent.document.clone(),
                    result: Err(error),
                },
                replication: None,
                replayed: false,
            }),
        }
    }

    /// Execute with captured authority, never rechecking Access after acceptance.
    /// This method does not insert a receipt and can compose across Documents.
    pub fn execute(
        &self,
        tx: &mut Transaction<'_>,
        admitted: AdmittedMutation,
    ) -> Result<MutationResult, StoreError> {
        let AdmittedMutation {
            before,
            intent,
            actor,
        } = admitted;
        let intent = &intent;
        let actor = actor.as_str();

        if matches!(
            intent.mutation.as_str(),
            "document.delete" | "document.archive" | "document.retry"
        ) {
            let mut lifecycle = self.lifecycle(tx, &intent.document)?;
            match intent.mutation.as_str() {
                "document.delete" => lifecycle.state = crate::lifecycle::State::Deleted,
                "document.archive" => lifecycle.state = crate::lifecycle::State::Archived,
                _ => lifecycle.blocked = None,
            }
            self.set_lifecycle(tx, &intent.document, &lifecycle)?;
            return Ok(MutationResult {
                completion: Completion {
                    id: intent.id,
                    document: intent.document.clone(),
                    result: Ok(
                        (lifecycle.state == crate::lifecycle::State::Active).then_some(before)
                    ),
                },
                replication: None,
                replayed: false,
            });
        }

        // Authorized: deterministic apply onto the latest state.
        let after = match self.registry.apply(&before, intent, actor) {
            Ok(next) => next,
            Err(error) => {
                let completion = Completion {
                    id: intent.id,
                    document: intent.document.clone(),
                    result: Err(error),
                };
                return Ok(MutationResult {
                    completion,
                    replication: None,
                    replayed: false,
                });
            }
        };
        if after.revision > i64::MAX as u64 {
            return Err(StoreError::Invalid);
        }
        let base_digest = digest(&before);
        let result_digest = digest(&after);
        let base_revision = before.revision;
        let revision = after.revision;

        let mut delta = Row::new();
        delta.insert("revision".into(), Value::Integer(after.revision as i64));
        delta.insert(
            "value".into(),
            Value::Text(serde_json::to_string(&after.value).map_err(|_| StoreError::Invalid)?),
        );
        tx.update(DOCUMENTS, &[Value::Text(intent.document.clone())], delta)?;

        let completion = Completion {
            id: intent.id,
            document: intent.document.clone(),
            result: Ok(Some(after)),
        };
        Ok(MutationResult {
            completion,
            replication: Some(Replication {
                intent: intent.clone(),
                actor: actor.into(),
                base_revision,
                base_digest,
                revision,
                result_digest,
            }),
            replayed: false,
        })
    }

    /// Commit a previously admitted wire mutation and its recovery receipt together.
    pub fn execute_recorded(
        &self,
        tx: &mut Transaction<'_>,
        lifetime: &str,
        admitted: AdmittedMutation,
    ) -> Result<MutationResult, StoreError> {
        validate_lifetime(lifetime)?;
        let actor = admitted.actor.clone();
        let intent = admitted.intent.clone();
        let result = self.execute(tx, admitted)?;
        insert_receipt(
            tx,
            lifetime,
            intent.id as i64,
            &actor,
            &intent,
            &result.completion,
        )?;
        Ok(result)
    }

    /// Recover an exact intent from this logical lifetime without executing it.
    pub fn recover(
        &self,
        tx: &mut Transaction<'_>,
        lifetime: &str,
        actor: &str,
        intent: &Intent,
    ) -> Result<Option<Completion>, StoreError> {
        validate_lifetime(lifetime)?;
        validate_actor(actor)?;
        validate_intent_shape(intent)?;
        let Some(row) = tx.get(RECEIPTS, &[lifetime.into(), (intent.id as i64).into()])? else {
            return Ok(None);
        };
        let stored = receipt_from_row(&row)?;
        if stored.actor != actor
            || stored.document != intent.document
            || stored.version != intent.version
            || stored.mutation != intent.mutation
            || stored.args != intent.args
        {
            return Err(StoreError::Invalid);
        }
        Ok(Some(stored.completion))
    }

    /// Reconcile one logical connection's active authorized holdings and receipts.
    ///
    /// Returns missing/stale active Documents, unchanged holdings, and, for
    /// each `pending` intent with a receipt in this lifetime, its stored
    /// completion (suppressed to `Ok(None)` when revoked). The union of snapshots
    /// and unchanged holdings is the complete desired client set, without filters.
    pub fn manifest(
        &self,
        tx: &mut Transaction<'_>,
        lifetime: &str,
        actor: &str,
        manifest: &Manifest,
    ) -> Result<Reconciliation, StoreError> {
        validate_lifetime(lifetime)?;
        validate_actor(actor)?;
        validate_pending(manifest)?;

        let accessible = self.access.accessible(tx, Some(actor), true)?;
        let mut documents = Vec::new();
        let mut unchanged = Vec::new();
        let mut allowed: BTreeSet<String> = BTreeSet::new();
        for entry in accessible {
            if entry.resource.kind != RESOURCE_KIND {
                continue;
            }
            let id = entry.resource.id;
            if self.lifecycle(tx, &id)?.state != crate::lifecycle::State::Active {
                continue;
            }
            let Some(row) = tx.get(DOCUMENTS, &[Value::Text(id.clone())])? else {
                // Access without a document row (created outside Document):
                // skip rather than invent a snapshot.
                continue;
            };
            let snapshot = snapshot_from_row(&row).map_err(|_| StoreError::Invalid)?;
            self.registry
                .validate(&snapshot)
                .map_err(|_| StoreError::Invalid)?;
            allowed.insert(id.clone());
            if let Some(holding) = manifest.holdings.iter().find(|holding| {
                holding.document == snapshot.id
                    && holding.version == snapshot.version
                    && holding.revision == snapshot.revision
                    && holding.digest == digest(&snapshot)
            }) {
                unchanged.push(holding.clone());
            } else {
                documents.push(snapshot);
            }
        }
        // `accessible` already sorts by kind then id; filtering preserves it.

        // Recover completions for pending intents in pending order.
        let mut seen: BTreeMap<u64, &Intent> = BTreeMap::new();
        let mut completed = Vec::new();
        for intent in &manifest.pending {
            if let Some(first) = seen.get(&intent.id) {
                if first.document != intent.document
                    || first.version != intent.version
                    || first.mutation != intent.mutation
                    || first.args != intent.args
                {
                    return Err(StoreError::Invalid);
                }
                continue;
            }
            seen.insert(intent.id, intent);
            let key = [
                Value::Text(lifetime.into()),
                Value::Integer(intent.id as i64),
            ];
            let Some(row) = tx.get(RECEIPTS, &key)? else {
                continue;
            };
            let stored = receipt_from_row(&row)?;
            if stored.actor != actor
                || stored.document != intent.document
                || stored.version != intent.version
                || stored.mutation != intent.mutation
                || stored.args != intent.args
            {
                return Err(StoreError::Invalid);
            }
            let mut completion = stored.completion;
            if let Ok(Some(_)) = &completion.result
                && !allowed.contains(&completion.document)
            {
                completion.result = Ok(None);
            }
            completed.push(completion);
        }
        Ok(Reconciliation {
            unchanged,
            documents,
            completed,
        })
    }

    /// Clear receipt rows for one lifetime. Document rows are never removed.
    ///
    /// Hosts call this when a logical connection expires. Committed documents
    /// stay committed; only deduplication state for the expired lifetime is
    /// dropped, so the same intent IDs may be reused on a fresh lifetime.
    pub fn expire(&self, tx: &mut Transaction<'_>, lifetime: &str) -> Result<(), StoreError> {
        validate_lifetime(lifetime)?;
        let rows = tx.find(RECEIPTS, "primary", &[Value::Text(lifetime.into())])?;
        for row in &rows {
            let intent = match row.get("intent") {
                Some(Value::Integer(value)) => *value,
                _ => return Err(StoreError::Invalid),
            };
            tx.delete(
                RECEIPTS,
                &[Value::Text(lifetime.into()), Value::Integer(intent)],
            )?;
        }
        Ok(())
    }

    /// Re-evaluate read authority for a stored completion.
    fn suppress_if_revoked(
        &self,
        tx: &mut Transaction<'_>,
        actor: &str,
        mut completion: Completion,
    ) -> Result<Completion, StoreError> {
        if completion.result.as_ref().is_ok_and(|slot| slot.is_some()) {
            let resource = Resource::new(RESOURCE_KIND, &completion.document)
                .map_err(|_| StoreError::Invalid)?;
            let role = self.access.role(tx, &resource, Some(actor), true)?;
            if !snap_access::allows(role, Role::Viewer) {
                completion.result = Ok(None);
            }
        }
        Ok(completion)
    }
}

fn validate_lifetime(lifetime: &str) -> Result<(), StoreError> {
    if lifetime.is_empty() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_actor(actor: &str) -> Result<(), StoreError> {
    if actor.is_empty() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_intent_shape(intent: &Intent) -> Result<(), StoreError> {
    if intent.id == 0 || intent.id > i64::MAX as u64 {
        return Err(StoreError::Invalid);
    }
    if !is_uuid(&intent.document) {
        return Err(StoreError::Invalid);
    }
    if intent.version.is_empty() || intent.mutation.is_empty() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_pending(manifest: &Manifest) -> Result<(), StoreError> {
    for intent in &manifest.pending {
        validate_intent_shape(intent)?;
    }
    Ok(())
}

/// UUID shape in any version: 8-4-4-4-12 hex, matching Access.
fn is_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn snapshot_row(snapshot: &Snapshot) -> Result<Row, StoreError> {
    let mut row = Row::new();
    row.insert("id".into(), Value::Text(snapshot.id.clone()));
    row.insert("kind".into(), Value::Text(snapshot.kind.clone()));
    row.insert("version".into(), Value::Text(snapshot.version.clone()));
    row.insert("revision".into(), Value::Integer(snapshot.revision as i64));
    row.insert(
        "value".into(),
        Value::Text(serde_json::to_string(&snapshot.value).map_err(|_| StoreError::Invalid)?),
    );
    Ok(row)
}

fn snapshot_from_row(row: &Row) -> Result<Snapshot, StoreError> {
    let id = text_field(row, "id")?.to_string();
    let kind = text_field(row, "kind")?.to_string();
    let version = text_field(row, "version")?.to_string();
    let revision = match row.get("revision") {
        Some(Value::Integer(value)) if *value >= 1 => *value as u64,
        _ => return Err(StoreError::Invalid),
    };
    let raw = text_field(row, "value")?;
    let value = serde_json::from_str(raw).map_err(|_| StoreError::Invalid)?;
    if !is_uuid(&id) || kind.is_empty() || version.is_empty() {
        return Err(StoreError::Invalid);
    }
    Ok(Snapshot {
        id,
        kind,
        version,
        revision,
        value,
    })
}

struct StoredReceipt {
    actor: String,
    document: String,
    version: String,
    mutation: String,
    args: serde_json::Value,
    completion: Completion,
}

fn receipt_from_row(row: &Row) -> Result<StoredReceipt, StoreError> {
    let actor = text_field(row, "actor")?.to_string();
    let document = text_field(row, "document")?.to_string();
    let version = text_field(row, "version")?.to_string();
    let mutation = text_field(row, "mutation")?.to_string();
    let args_raw = text_field(row, "args")?;
    let args = serde_json::from_str(args_raw).map_err(|_| StoreError::Invalid)?;
    let completion_raw = text_field(row, "completion")?;
    let completion = serde_json::from_str(completion_raw).map_err(|_| StoreError::Invalid)?;
    if actor.is_empty() || !is_uuid(&document) || version.is_empty() || mutation.is_empty() {
        return Err(StoreError::Invalid);
    }
    Ok(StoredReceipt {
        actor,
        document,
        version,
        mutation,
        args,
        completion,
    })
}

fn insert_receipt(
    tx: &mut Transaction<'_>,
    lifetime: &str,
    intent_key: i64,
    actor: &str,
    intent: &Intent,
    completion: &Completion,
) -> Result<(), StoreError> {
    let mut row = Row::new();
    row.insert("lifetime".into(), Value::Text(lifetime.into()));
    row.insert("intent".into(), Value::Integer(intent_key));
    row.insert("actor".into(), Value::Text(actor.into()));
    row.insert("document".into(), Value::Text(intent.document.clone()));
    row.insert("version".into(), Value::Text(intent.version.clone()));
    row.insert("mutation".into(), Value::Text(intent.mutation.clone()));
    row.insert(
        "args".into(),
        Value::Text(serde_json::to_string(&intent.args).map_err(|_| StoreError::Invalid)?),
    );
    row.insert(
        "completion".into(),
        Value::Text(serde_json::to_string(completion).map_err(|_| StoreError::Invalid)?),
    );
    tx.insert(RECEIPTS, row)?;
    Ok(())
}

fn text_field<'a>(row: &'a Row, column: &str) -> Result<&'a str, StoreError> {
    match row.get(column) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(StoreError::Invalid),
    }
}
