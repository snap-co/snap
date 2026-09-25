# Architecture

Snap separates capability contracts from their providers. Applications select
implementations and explicitly publish operations. Hosts own execution and IO.

## Ownership

Protocol declares invocation contracts; Transport provides dispatch. Identity
declares authentication contracts; Passport implements them. Store declares local
persistence. Planned pairs include Access/Acl, Document/Snapshot, and Blob/Bucket;
those capabilities are not implemented by this spike.

Contract consumers must not depend on providers. Keep portable contracts,
application behavior, runtime providers, and client controllers `no_std` with
`alloc`. Platforms own IO, clocks, randomness, expensive crypto, and task lifetime.
Use Rust modules to organize behavior; extract a crate when an independent consumer
or enforceable dependency/portability boundary requires it.

`apps/authy/native/src/lib.rs` is the reference composition. It selects Passport,
SQLite Store, NoCache, crypto, and cookie projection. Carrier bindings in
`platforms/web` own HTTP/WebSocket envelopes, sequence framing, and close codes.
Portable operations have no carrier classification. A single operation can have
multiple explicit bindings; declaring a table or importing a type publishes nothing.

Shared client controllers consume normalized outcomes and disconnect reasons.
Native and browser adapters drive those controllers; TypeScript adapts WASM values
to Promises and immutable observations; React renders them.

## Host-driven execution

`Provider::invoke` creates an owned future with capability-specific context and
output. The host polls it and performs external work. Immediate providers need not
suspend. This replaces hand-maintained workflow stages without giving providers
control of an executor or introducing capability-specific branches in the scheduler.

Keep blocking IO out of portable polling and release guards before suspension.
Document admission, cancellation, ordering, late results, shutdown, and uncertain
write outcomes beside the interface that promises them. Futures are in-memory
continuations, not durable checkpoints.

The web host retains admitted continuations after an HTTP observer leaves, including
their capacity and later delivery effects. Shutdown drops remaining continuations;
an already-started blocking Store transaction can still finish. Process failure
does not recover or replay the workflow.

## Storage and authority

Store is local persistence, independently useful to Passport and future consumers
without forcing their records into Document's tree/synchronization model. Modules
own logical schemas and namespaces; application composition registers them together.
Namespaces express ownership, not end-user authorization. Published operations own
permission checks.

The authoritative transaction and advisory-cache contracts live beside the traits
in `crates/store/src/lib.rs`. Memory and SQLite implement the same atomic semantics;
only SQLite is durable. Native Store executes work on blocking host workers.
Backend substitution requires the same guarantees, not merely a database adapter.

SQLite maps tables to quoted `namespace_name` identifiers and registers schemas
atomically. Passport migrates original `credentials`, `sessions`, and `settings`
tables into `snap_identity`, preserving rows and the signing key. Incompatible
schema changes require an explicit migration design. There is no Postgres backend.

A record cache holds advisory snapshots whose residency does not establish freshness.
Authorization and dependent writes require current transactional authority. Authy
selects NoCache. Retry receipts describe operation outcomes and have a different
lifetime; they are not record-cache entries.

## Port a capability

1. Select the TypeScript revision and observable consumer flow, including failures,
   cancellation, recovery, and persisted-data compatibility. Read the implementation
   as well as its declarations.
2. Assign contract, provider, authorization, carrier, and host ownership. Distinguish
   published operations from trusted administrative methods and server-only work.
3. Implement portable behavior with typed external-work interfaces owned by the
   concern using them. Let composition connect providers to executors.
4. Declare required coherent reads, atomic writes, and migrations through Store.
   Cross-module transactions use module-owned interfaces, not private table access.
5. Verify the usable flow through SDK or Protocol contracts under [TESTING.md](TESTING.md).

Record enduring translation decisions beside the affected contract. The selected
reference revision is `9689a8ed3108f58233721c2000d2b9ea96259fe7` in `~/code/bod/snap`.
Healthy covers anonymous `health.up`, completion/input/route behavior, and Build
discovery. Doctor retains the readiness exception to exact Build matching.
[Authy](apps/authy/CONTRACT.md) defines its additional compatibility subset.

## Dependency enforcement

Local/path packages declare `package.metadata.snap.role`. Normal dependencies obey:

| Role | Allowed local dependencies |
| --- | --- |
| contract | contract |
| core | contract, core |
| application | contract, core, application |
| platform | contract, core, platform |
| binding | contract, core, platform, binding |
| tool | contract, core, platform, binding, tool |
| composition | all roles |

Development/build dependencies may use host code, but shared packages cannot depend
on application/composition packages through any dependency kind. Structural checks
resolve host and both WASM targets with all features; contract/core/application
libraries compile for `wasm32v1-none`. Every feature of a portable package must stay
portable. Registry/git dependencies do not require Snap roles.
