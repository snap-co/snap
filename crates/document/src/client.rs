//! Optimistic document client SDK: authoritative state plus an ordered journal
//! of pending mutation intents with the projected view replayed over it.
//!
//! Hosts own IO, clocks and logical connection lifetimes; this module is
//! portable `no_std` with `alloc` and performs no IO. The shared
//! [`Registry`](crate::Registry) supplies the pure deterministic mutation
//! behavior used identically by the authority, optimistic replay and remote
//! replication.
//!
//! Non-obvious guarantees promised beside this interface:
//!
//! * The visible [`view`](Client::view) is always authoritative state with the
//!   whole [`pending`](Client::pending) journal replayed in enqueue order. A
//!   local append, a completion and an incoming replication each recompute that
//!   projection and advance [`revision`](Client::revision) exactly once when the
//!   publication changes. `Accepted` only paces submission and never changes
//!   the view, so it never advances the revision.
//! * Completion effects and removal of that intent's journal entry publish as
//!   one coherent view: the completed intent is never applied twice and the
//!   remaining entries replay in order over the new base.
//! * Submission is paced by acceptance acknowledgements, not completions. At
//!   most one intent awaits `Accepted`; the next submission may leave after the
//!   preceding `Accepted` while its completion is still pending. Transport
//!   invocation IDs are assigned separately by the carrier (see `wire.rs`); the
//!   [`Intent::id`] values managed here are stable within one logical
//!   connection, nonzero, never reused, and exhaustion reports
//!   [`Error::Invalid`].
//! * Intent replay requires a matching authoritative base and compatible
//!   mutation behavior. Any base/revision/digest or compatibility mismatch is
//!   an explicit [`Outcome::NeedManifest`] carrying the outgoing [`Manifest`]
//!   for authoritative resynchronization. Divergent replication is never
//!   applied silently.
//! * A [`Reconciliation`] is an authoritative replacement of ALL permitted
//!   holdings. Receipts carried in the same reconciliation only remove journal
//!   entries; their snapshots never replace the newer replacement documents,
//!   so recovered receipts cannot roll authoritative state backwards.
//!   Reconciliations arrive both unsolicited (Access adds holdings, or the
//!   holdings refresh queued after the originator's own completion) and
//!   correlated (initial, surviving-reconnect, or recovery answers to a
//!   presented [`Manifest`]). Only a correlated reconciliation requeues work:
//!   an unsolicited replacement preserves acceptance pacing and never resends
//!   work. Wire correlation distinguishes `Holdings` pushes from `Manifest`
//!   results. Holdings received during recovery are deferred until its result;
//!   they cannot open the recovery gate or double-apply committed pending work.
//!   The host also filters revoked queued frames
//!   before each socket send, so tombstones here are backup defense.
//! * A surviving reconnect retains the journal: the host calls
//!   [`begin_reconnect`](Client::begin_reconnect), presents
//!   [`manifest`](Client::manifest), sends no mutations until the
//!   reconciliation arrives, then resubmits the unknown intents with their
//!   original IDs. Receipt deduplication happens only through that
//!   reconciliation.
//! * [`Reset`](crate::ServerMessage::Reset) (logical-connection expiry) clears
//!   authoritative state, the projected view, pending work and revocation
//!   tombstones, opens a fresh manifest exchange, and never replays writes
//!   from the expired lifetime. Server commits from that lifetime stay
//!   committed.
//! * [`Removed`](crate::ServerMessage::Removed) clears authoritative data and
//!   every pending intent for the listed documents, records revocation
//!   tombstones, and rejects both new local edits (`Denied`) and queued remote
//!   delivery (`Denied`) for those documents until the next reconciliation
//!   clears the tombstones. Reconciliation itself is the full permitted set,
//!   so clearing there cannot reintroduce revoked data.
//! * Replay errors never silently drop the journal: the journal is retained and
//!   the error is surfaced as `Err`, or alongside recovered outcomes in
//!   `Reconciled::replay_error`, with the view holding the authoritative base
//!   plus the successfully replayed prefix.

use crate::{ClientMessage, Completion, Replication, ServerMessage, digest};
use crate::{Error, Holding, Intent, Manifest, Reconciliation, Registry, Snapshot};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

/// Observable result of [`Client::handle`].
///
/// `NeedManifest` is the only divergence path: the caller must send
/// [`ClientMessage::Manifest`] with the contained manifest and await the
/// correlated reconciliation, which alone requeues the unresolved intents.
/// An unsolicited holdings refresh never requeues. All other `Ok` variants
/// describe an applied publication; `Err` describes a replay or protocol
/// failure with the journal retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// An unsolicited holdings refresh arrived while a correlated recovery is
    /// pending. Its replacement will be covered by that ordered manifest result.
    Deferred,
    /// Acceptance pacing only; the view is unchanged.
    Accepted { id: u64 },
    /// Authoritative effects applied and the intent's entry removed.
    Completed { id: u64 },
    /// The write committed but current authority forbids its contents. The
    /// document and every pending intent for it were dropped.
    Forbidden { id: u64, document: String },
    /// The server rejected the write. Only that entry was removed; later
    /// intents were preserved and replayed.
    Rejected { id: u64, error: Error },
    /// A remote intent was verified and applied, then local pending rebased.
    Replicated { document: String, revision: u64 },
    /// An authoritative replacement was installed. `completed` counts journal
    /// entries removed by carried receipts (their snapshots were ignored).
    /// `outcomes` reports Completed, Rejected or Forbidden for each recovered
    /// pending intent, so losing its original completion never hides rejection.
    /// A replay failure retains unresolved intents and is reported alongside
    /// the recovered outcomes rather than hiding them behind a returned error.
    Reconciled {
        documents: usize,
        completed: usize,
        outcomes: Vec<Outcome>,
        replay_error: Option<Error>,
    },
    /// Revocation applied; tombstones now block reintroduction.
    Removed { documents: Vec<String> },
    /// Expiry applied; all local state was cleared.
    Reset,
    /// Base, digest, revision or compatibility mismatch. State is unchanged;
    /// send the contained manifest for authoritative resynchronization.
    NeedManifest { manifest: Manifest, error: Error },
    /// A completion for an intent already removed (for example a recovered
    /// receipt older than the installed replacement). No state changed and
    /// nothing rolled back.
    Ignored { id: u64 },
}

/// Optimistic document client.
///
/// Holds the verified actor identity, the authoritative snapshots, the ordered
/// pending intent journal, and the projected view. All behavior is synchronous
/// and IO-free; the host drives [`next_submission`](Client::next_submission)
/// and feeds [`ServerMessage`]s into [`handle`](Client::handle).
#[derive(Clone, Debug)]
pub struct Client {
    actor: String,
    authoritative: BTreeMap<String, Snapshot>,
    view: BTreeMap<String, Snapshot>,
    pending: Vec<Intent>,
    acked: BTreeSet<u64>,
    awaiting_ack: Option<u64>,
    revoked: BTreeSet<String>,
    reconciling: bool,
    recovery_pending: bool,
    revision: u64,
    next_id: u64,
}

impl Client {
    /// Create a client for `actor` (the transport-verified identity string).
    ///
    /// Starts ready with empty holdings and the submission gate open. A fresh
    /// client presents an empty [`manifest`](Client::manifest); a surviving
    /// reconnect must call [`begin_reconnect`](Client::begin_reconnect) first
    /// so no mutations leave until the reconciliation arrives. Intent IDs
    /// start at 1 and increase monotonically within this logical connection.
    pub fn new(actor: String) -> Self {
        Self {
            actor,
            authoritative: BTreeMap::new(),
            view: BTreeMap::new(),
            pending: Vec::new(),
            acked: BTreeSet::new(),
            awaiting_ack: None,
            revoked: BTreeSet::new(),
            reconciling: false,
            recovery_pending: false,
            revision: 0,
            next_id: 1,
        }
    }

    /// The verified actor this client replays local intents as.
    pub fn actor(&self) -> &str {
        &self.actor
    }

    /// Monotonically increasing publication counter.
    ///
    /// Advances once per applied local edit, completion (including rejection
    /// and forbidden), replication, reconciliation, removal and reset. `Accepted`
    /// and ignored stale receipts never advance it, so reactive consumers can
    /// treat a change as "the projected view or holdings changed". Wrapping is
    /// impossible in practice; saturation keeps the counter monotone.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Authoritative snapshots without optimistic replay.
    pub fn authoritative(&self) -> &BTreeMap<String, Snapshot> {
        &self.authoritative
    }

    /// Projected view: authoritative state with the whole pending journal
    /// replayed in order. Consumers read this, never `authoritative` plus a
    /// private overlay.
    pub fn view(&self) -> &BTreeMap<String, Snapshot> {
        &self.view
    }

    /// Ordered pending intent journal (oldest first).
    pub fn pending(&self) -> &[Intent] {
        &self.pending
    }

    /// The intent ID currently awaiting `Accepted`, if any.
    pub fn awaiting_ack(&self) -> Option<u64> {
        self.awaiting_ack
    }

    /// True after [`begin_reconnect`](Client::begin_reconnect) or expiry until
    /// the correlated reconciliation arrives. While true,
    /// [`enqueue`](Client::enqueue) reports `Protocol` and
    /// [`next_submission`](Client::next_submission) returns `None`.
    /// An unsolicited holdings refresh never opens this gate on its own;
    /// only the correlated answer to the presented manifest does.
    pub fn is_reconciling(&self) -> bool {
        self.reconciling
    }

    /// True after [`Outcome::NeedManifest`] until the correlated
    /// reconciliation arrives. While set, the next reconciliation requeues
    /// unresolved intents with their original IDs; an unsolicited refresh
    /// with this unset preserves acceptance pacing instead.
    pub fn needs_recovery(&self) -> bool {
        self.recovery_pending
    }

    /// Projected snapshot for one document, if held.
    pub fn get(&self, document: &str) -> Option<&Snapshot> {
        self.view.get(document)
    }

    /// Present current holdings plus the retained journal.
    ///
    /// Holdings carry `(document, version, revision, digest)` for every
    /// authoritative snapshot in sorted document order; `pending` clones the
    /// ordered journal with stable IDs. Fresh clients present an empty
    /// manifest; surviving reconnects present retained state after
    /// [`begin_reconnect`](Client::begin_reconnect).
    pub fn manifest(&self) -> Manifest {
        self.current_manifest()
    }

    /// Enter the surviving-reconnect gate.
    ///
    /// Clears pacing (`awaiting_ack` and accepted marks) so the retained
    /// intents resubmit with their original IDs only after the correlated
    /// reconciliation dedups them. The journal, authoritative state and
    /// tombstones are retained. No revision advance: presenting the same
    /// holdings is not a new publication. Any armed recovery is subsumed:
    /// the correlated reconnect answer is the recovery.
    pub fn begin_reconnect(&mut self) {
        self.reconciling = true;
        self.recovery_pending = false;
        self.awaiting_ack = None;
        self.acked.clear();
    }

    /// Append a local mutation intent and immediately replay the journal.
    ///
    /// The intent version is taken from the projected base for `document`;
    /// unknown documents report `NotFound`, revoked documents report `Denied`,
    /// and a closed reconciliation gate reports `Protocol`. IDs are assigned
    /// here and never reused; exhaustion reports `Invalid`. The new intent is
    /// retained even when replay of the enlarged journal fails; the error is
    /// then surfaced and the view holds the authoritative base plus the
    /// successfully replayed prefix.
    pub fn enqueue(
        &mut self,
        registry: &Registry,
        document: &str,
        mutation: &str,
        args: serde_json::Value,
    ) -> Result<u64, Error> {
        if self.reconciling || self.recovery_pending {
            return Err(Error::Protocol);
        }
        if document.is_empty() || mutation.is_empty() {
            return Err(Error::Invalid);
        }
        if self.revoked.contains(document) {
            return Err(Error::Denied);
        }
        let base = self
            .view
            .get(document)
            .or_else(|| self.authoritative.get(document))
            .ok_or(Error::NotFound)?;
        let version = base.version.clone();
        let id = self.next_id;
        if id == 0 {
            return Err(Error::Invalid);
        }
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        let intent = Intent {
            id,
            document: document.into(),
            version,
            mutation: mutation.into(),
            args,
        };
        self.pending.push(intent);
        match self.rebuild_view(registry) {
            Ok(()) => {
                self.bump();
                Ok(id)
            }
            Err(error) => {
                self.bump();
                Err(error)
            }
        }
    }

    /// Next intent to submit, paced by acceptance.
    ///
    /// Returns `None` while reconciling or while one intent still awaits
    /// `Accepted`, and marks the returned intent as awaiting `Accepted`. The
    /// next submission becomes available after the preceding `Accepted`
    /// without waiting for its completion. Already-accepted intents still
    /// awaiting completion are skipped, never resent on this connection.
    pub fn next_submission(&mut self) -> Option<ClientMessage> {
        if self.reconciling || self.recovery_pending {
            return None;
        }
        if self.awaiting_ack.is_some() {
            return None;
        }
        for intent in &self.pending {
            if self.acked.contains(&intent.id) {
                continue;
            }
            self.awaiting_ack = Some(intent.id);
            return Some(ClientMessage::Mutate(intent.clone()));
        }
        None
    }

    /// Apply one server message and publish a single coherent view.
    ///
    /// Completion effects and removal of the completed entry publish together;
    /// remaining entries replay in order. The originator's completion is
    /// queued before any same-commit holdings refresh, so handling them in
    /// arrival order never double-applies. Replication is verified (base
    /// revision/digest, compatible behavior, result revision/digest) and never
    /// applied on mismatch: the journal is retained, recovery is armed, and
    /// `NeedManifest` requests resynchronization. A correlated reconciliation
    /// replaces ALL holdings, removes only its carried receipts, and requeues
    /// unresolved intents; an unsolicited holdings refresh installs the same
    /// replacement but preserves acceptance pacing. Removal records tombstones
    /// that block reintroduction (the host also filters revoked queued frames
    /// before each send). Reset clears everything for a fresh exchange.
    pub fn handle(
        &mut self,
        registry: &Registry,
        message: ServerMessage,
    ) -> Result<Outcome, Error> {
        match message {
            ServerMessage::Accepted { id } => self.handle_accepted(id),
            ServerMessage::Completed(completion) => self.handle_completed(registry, completion),
            ServerMessage::Manifest(reconciliation) => {
                self.handle_reconciliation(registry, reconciliation)
            }
            ServerMessage::Holdings(documents) => {
                if self.reconciling || self.recovery_pending {
                    return Ok(Outcome::Deferred);
                }
                self.handle_reconciliation(
                    registry,
                    Reconciliation {
                        documents,
                        completed: Vec::new(),
                    },
                )
            }
            ServerMessage::Replication(replication) => {
                self.handle_replication(registry, replication)
            }
            ServerMessage::Removed(documents) => self.handle_removed(registry, documents),
            ServerMessage::Reset => Ok(self.handle_reset()),
        }
    }

    fn handle_accepted(&mut self, id: u64) -> Result<Outcome, Error> {
        if id == 0 {
            return Err(Error::Protocol);
        }
        if self.awaiting_ack == Some(id) {
            if !self.pending.iter().any(|intent| intent.id == id) {
                return Err(Error::Protocol);
            }
            self.awaiting_ack = None;
            self.acked.insert(id);
            Ok(Outcome::Accepted { id })
        } else if self.acked.contains(&id) {
            Ok(Outcome::Accepted { id })
        } else {
            Err(Error::Protocol)
        }
    }

    fn handle_completed(
        &mut self,
        registry: &Registry,
        completion: Completion,
    ) -> Result<Outcome, Error> {
        let Some(index) = self
            .pending
            .iter()
            .position(|intent| intent.id == completion.id)
        else {
            // Already removed (for example a recovered receipt older than the
            // installed replacement). Ignore without rolling anything back.
            return Ok(Outcome::Ignored { id: completion.id });
        };
        let intent = self.pending[index].clone();
        if completion.document != intent.document {
            return Err(Error::Protocol);
        }
        if self.revoked.contains(&intent.document) {
            self.pending.remove(index);
            self.acked.remove(&completion.id);
            if self.awaiting_ack == Some(completion.id) {
                self.awaiting_ack = None;
            }
            self.rebuild_view(registry)?;
            self.bump();
            return Ok(Outcome::Rejected {
                id: completion.id,
                error: Error::Denied,
            });
        }
        match completion.result {
            Ok(Some(snapshot)) => {
                if snapshot.id != intent.document || snapshot.version != intent.version {
                    return Ok(self.need_manifest(Error::Incompatible));
                }
                if let Err(error) = registry.validate(&snapshot) {
                    return Ok(self.need_manifest(error));
                }
                let current = self
                    .authoritative
                    .get(&intent.document)
                    .map(|held| held.revision)
                    .unwrap_or(0);
                self.pending.remove(index);
                self.acked.remove(&completion.id);
                if self.awaiting_ack == Some(completion.id) {
                    self.awaiting_ack = None;
                }
                if snapshot.revision <= current {
                    // Stale receipt (older than the installed replacement):
                    // drop the entry but keep the newer authoritative state.
                    match self.rebuild_view(registry) {
                        Ok(()) => {
                            self.bump();
                            Ok(Outcome::Completed { id: completion.id })
                        }
                        Err(error) => {
                            self.bump();
                            Err(error)
                        }
                    }
                } else {
                    self.authoritative.insert(intent.document.clone(), snapshot);
                    match self.rebuild_view(registry) {
                        Ok(()) => {
                            self.bump();
                            Ok(Outcome::Completed { id: completion.id })
                        }
                        Err(error) => {
                            self.bump();
                            Err(error)
                        }
                    }
                }
            }
            Ok(None) => {
                // Committed but forbidden: drop the document and every pending
                // intent for it, since none can be projected without a base.
                let document = intent.document.clone();
                self.pending.retain(|queued| queued.document != document);
                let remaining: BTreeSet<u64> =
                    self.pending.iter().map(|queued| queued.id).collect();
                self.acked.retain(|id| remaining.contains(id));
                if let Some(awaiting) = self.awaiting_ack
                    && !remaining.contains(&awaiting)
                {
                    self.awaiting_ack = None;
                }
                self.authoritative.remove(&document);
                match self.rebuild_view(registry) {
                    Ok(()) => {
                        self.bump();
                        Ok(Outcome::Forbidden {
                            id: completion.id,
                            document,
                        })
                    }
                    Err(error) => {
                        self.bump();
                        Err(error)
                    }
                }
            }
            Err(server_error) => {
                self.pending.remove(index);
                self.acked.remove(&completion.id);
                if self.awaiting_ack == Some(completion.id) {
                    self.awaiting_ack = None;
                }
                match self.rebuild_view(registry) {
                    Ok(()) => {
                        self.bump();
                        Ok(Outcome::Rejected {
                            id: completion.id,
                            error: server_error,
                        })
                    }
                    Err(error) => {
                        self.bump();
                        Err(error)
                    }
                }
            }
        }
    }

    fn handle_replication(
        &mut self,
        registry: &Registry,
        replication: Replication,
    ) -> Result<Outcome, Error> {
        let document = replication.intent.document.clone();
        if document.is_empty() || replication.intent.id == 0 {
            return Ok(self.need_manifest(Error::Incompatible));
        }
        if self.revoked.contains(&document) {
            // Queued delivery must not reintroduce revoked data. The host
            // also filters revoked frames before each send.
            return Err(Error::Denied);
        }
        let Some(base) = self.authoritative.get(&document).cloned() else {
            return Ok(self.need_manifest(Error::NotFound));
        };
        if base.revision != replication.base_revision || digest(&base) != replication.base_digest {
            return Ok(self.need_manifest(Error::Diverged("base mismatch".into())));
        }
        let applied = match registry.apply(&base, &replication.intent, &replication.actor) {
            Ok(next) => next,
            Err(error) => {
                return Ok(self.need_manifest(error));
            }
        };
        if applied.revision != replication.revision || digest(&applied) != replication.result_digest
        {
            return Ok(self.need_manifest(Error::Diverged("result mismatch".into())));
        }
        self.authoritative.insert(document.clone(), applied);
        match self.rebuild_view(registry) {
            Ok(()) => {
                self.bump();
                Ok(Outcome::Replicated {
                    document,
                    revision: replication.revision,
                })
            }
            Err(error) => {
                self.bump();
                Err(error)
            }
        }
    }

    fn handle_reconciliation(
        &mut self,
        registry: &Registry,
        reconciliation: Reconciliation,
    ) -> Result<Outcome, Error> {
        {
            let mut seen = BTreeSet::new();
            for snapshot in &reconciliation.documents {
                if !seen.insert(snapshot.id.clone()) {
                    return Err(Error::Invalid);
                }
                registry.validate(snapshot)?;
            }
            for completion in &reconciliation.completed {
                if completion.id == 0 {
                    return Err(Error::Protocol);
                }
            }
        }
        // Requested (surviving reconnect or recovery) versus unsolicited
        // (Access holdings gain or the refresh after our own completion).
        // Only the correlated answer requeues unresolved intents and opens
        // the gate; an unsolicited refresh preserves acceptance pacing.
        let requested = self.reconciling || self.recovery_pending;
        let mut authoritative = BTreeMap::new();
        for snapshot in reconciliation.documents {
            authoritative.insert(snapshot.id.clone(), snapshot);
        }
        self.authoritative = authoritative;
        let mut removed = 0_usize;
        let mut outcomes = Vec::new();
        let mut forbidden = BTreeSet::new();
        for completion in &reconciliation.completed {
            if let Some(index) = self
                .pending
                .iter()
                .position(|intent| intent.id == completion.id)
            {
                // Receipts only dedup the journal; their snapshots never
                // replace the newer replacement documents above.
                self.pending.remove(index);
                outcomes.push(match &completion.result {
                    Ok(Some(_)) => Outcome::Completed { id: completion.id },
                    Ok(None) => {
                        forbidden.insert(completion.document.clone());
                        Outcome::Forbidden {
                            id: completion.id,
                            document: completion.document.clone(),
                        }
                    }
                    Err(error) => Outcome::Rejected {
                        id: completion.id,
                        error: error.clone(),
                    },
                });
                self.acked.remove(&completion.id);
                if self.awaiting_ack == Some(completion.id) {
                    self.awaiting_ack = None;
                }
                removed += 1;
            }
        }
        // The replacement is newer than the receipts. Discard dependent intents
        // only when the latest authorized holdings still omit a forbidden doc.
        self.pending.retain(|intent| {
            !forbidden.contains(&intent.document)
                || self.authoritative.contains_key(&intent.document)
        });
        let remaining: BTreeSet<u64> = self.pending.iter().map(|intent| intent.id).collect();
        self.acked.retain(|id| remaining.contains(id));
        if self.awaiting_ack.is_some_and(|id| !remaining.contains(&id)) {
            self.awaiting_ack = None;
        }
        if requested {
            // Correlated answer: requeue what the server has not resolved by
            // forgetting acceptance marks; the same stable IDs resubmit and
            // the server dedups committed ones via receipts.
            self.awaiting_ack = None;
            self.acked.clear();
            self.reconciling = false;
            self.recovery_pending = false;
        }
        // Tombstones lift only for documents the replacement actually
        // permits again; still-absent documents stay blocked so queued
        // delivery cannot reintroduce them.
        let permitted: BTreeSet<String> = self.authoritative.keys().cloned().collect();
        self.revoked
            .retain(|document| !permitted.contains(document));
        let documents = self.authoritative.len();
        let replay_error = self.rebuild_view(registry).err();
        self.bump();
        Ok(Outcome::Reconciled {
            documents,
            completed: removed,
            outcomes,
            replay_error,
        })
    }

    fn handle_removed(
        &mut self,
        registry: &Registry,
        documents: Vec<String>,
    ) -> Result<Outcome, Error> {
        if documents.is_empty() {
            return Ok(Outcome::Removed {
                documents: Vec::new(),
            });
        }
        for document in &documents {
            if document.is_empty() {
                return Err(Error::Invalid);
            }
        }
        let targets: BTreeSet<String> = documents.iter().cloned().collect();
        for document in &targets {
            self.authoritative.remove(document);
            self.revoked.insert(document.clone());
        }
        self.pending
            .retain(|intent| !targets.contains(&intent.document));
        let remaining: BTreeSet<u64> = self.pending.iter().map(|intent| intent.id).collect();
        self.acked.retain(|id| remaining.contains(id));
        if let Some(awaiting) = self.awaiting_ack
            && !remaining.contains(&awaiting)
        {
            self.awaiting_ack = None;
        }
        match self.rebuild_view(registry) {
            Ok(()) => {
                self.bump();
                Ok(Outcome::Removed { documents })
            }
            Err(error) => {
                self.bump();
                Err(error)
            }
        }
    }

    fn handle_reset(&mut self) -> Outcome {
        // Expired lifetime: drop authoritative state, the projected view,
        // pending work and tombstones. Expired writes are never replayed; the
        // next exchange starts from a fresh (empty) manifest.
        self.authoritative.clear();
        self.view.clear();
        self.pending.clear();
        self.acked.clear();
        self.awaiting_ack = None;
        self.revoked.clear();
        self.reconciling = true;
        self.recovery_pending = false;
        self.bump();
        Outcome::Reset
    }

    /// Arm recovery and return the outgoing manifest for resynchronization.
    /// The next correlated reconciliation requeues unresolved intents;
    /// unsolicited refreshes in the meantime preserve pacing.
    fn need_manifest(&mut self, error: Error) -> Outcome {
        self.recovery_pending = true;
        Outcome::NeedManifest {
            manifest: self.current_manifest(),
            error,
        }
    }

    fn current_manifest(&self) -> Manifest {
        let mut holdings = Vec::with_capacity(self.authoritative.len());
        for snapshot in self.authoritative.values() {
            holdings.push(Holding {
                document: snapshot.id.clone(),
                version: snapshot.version.clone(),
                revision: snapshot.revision,
                digest: digest(snapshot),
            });
        }
        Manifest {
            holdings,
            pending: self.pending.clone(),
        }
    }

    fn rebuild_view(&mut self, registry: &Registry) -> Result<(), Error> {
        let mut projected = self.authoritative.clone();
        for intent in &self.pending {
            if self.revoked.contains(&intent.document) {
                self.view = projected;
                return Err(Error::Denied);
            }
            let Some(base) = projected.get(&intent.document).cloned() else {
                self.view = projected;
                return Err(Error::NotFound);
            };
            match registry.apply(&base, intent, &self.actor) {
                Ok(next) => {
                    projected.insert(intent.document.clone(), next);
                }
                Err(error) => {
                    self.view = projected;
                    return Err(error);
                }
            }
        }
        self.view = projected;
        Ok(())
    }

    fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}
