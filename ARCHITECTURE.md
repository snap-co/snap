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

`platforms/native` and `platforms/workers` are distinct execution hosts. Rust is
their implementation language, not their platform identity. Workers compositions
live beside the native/wasm compositions in Healthy and Authy. The signed-cookie
codec and web reply projection live in `platforms/web` and serve both hosts.

`snap-http` declares owned requests/responses for application-selected standard HTTP
endpoints. Its native and Workers adapters retain bounded, thread-local calls without
adding Snap Build negotiation or completion envelopes. Applications own origin,
content-type and authorization policy. OIDC uses this carrier because its standard
redirect/form/token protocol cannot be represented by Snap's operation envelope.
`snap-oidc` owns issuer grant transitions and claim policy. Its injected Accounts
interface supplies Passport-owned transaction guards; host crypto supplies RS256.

Chatty selects standard HTTP for its BFF session and thread UI. `snap-http::client`
declares bounded outgoing streams; hosts execute network requests with explicit
deadlines and no automatic retry. `snap-llm` interprets Responses streams and opaque
reasoning replay without owning HTTP execution or tool IO. Chatty owns session,
thread and turn records plus tool policy. Native supplies confined workspace file
access; Workers supplies service-bound Authy calls and disables native file tools.
The React client renders these HTTP observations directly. It does not create a
WASM binding for a Snap Protocol controller it does not consume.

## Host-driven execution

`Provider::invoke` creates an owned future with capability-specific context and
output. The host polls it and performs external work. Immediate providers need not
suspend. This replaces hand-maintained workflow stages without giving providers
control of an executor or introducing capability-specific branches in the scheduler.

Providers, Store/cache handles, crypto handles, and their futures can be thread-local.
Their contracts do not require `Send` or `Sync`. Native uses a current-thread Tokio
runtime and a local dispatcher that interleaves owned invocation futures. Blocking
Store and crypto requests still cross into host workers using thread-safe owned
inputs/results. Native Store serializes transactions under its connection lock;
it does not dedicate a thread to each invocation or promise a fixed thread count.
Workers uses its event loop and `workers-rs`; no Tokio executor is involved.

Keep blocking IO out of portable polling and release guards before suspension.
Document admission, cancellation, ordering, late results, shutdown, and uncertain
write outcomes beside the interface that promises them. Futures are in-memory
continuations, not durable checkpoints.

The web host retains admitted continuations after an HTTP observer leaves, including
their capacity and later delivery effects. Shutdown drops remaining continuations;
an already-started blocking Store transaction can still finish. Process failure
does not recover or replay the workflow.

The Workers Durable Object host polls accepted futures independently of the HTTP
response waiter. `State::wait_until` registers the future, but does not extend a
Durable Object's lifetime. Ongoing work and pending IO keep the object active under
the platform lifecycle. The five-second Snap response deadline does not cancel
admitted work. Runtime termination can interrupt it; there is no durable workflow
replay. Host lifetime and delivery mechanisms must be documented separately from
the portable operation semantics.

## Storage and authority

Store is local persistence, independently useful to Passport and future consumers
without forcing their records into Document's tree/synchronization model. Modules
own logical schemas and namespaces; application composition registers them together.
Namespaces express ownership, not end-user authorization. Published operations own
permission checks.

Authy's account profile is an application-owned versioned JSON document in Store.
It does not imply the planned Document/Snapshot replication capability. Passport's
trusted enrollment callback constructs app-owned statements for the same transaction
as credential/session creation. Passport also supplies current-session guards and
server-only identity projections so OIDC never reads its private credential rows.

The authoritative transaction and advisory-cache contracts live beside the traits
in `crates/store/src/lib.rs`. Memory and SQLite implement the same atomic semantics;
only SQLite is durable. Native Store executes work on blocking host workers.
Backend substitution requires the same guarantees, not merely a database adapter.

Workers Store uses SQLite inside one Durable Object. All namespaces participating
in a transaction must be registered in that object. Guards and statements execute
in `transactionSync` without suspension; results are fully consumed there, and
the future waits for `storage.sync()` before returning success. Integer bindings
and results use decimal text with SQL casts to preserve all signed 64-bit values
across the JavaScript interface. Schema/transaction validation is shared with
native. Unregistered physical tables and incompatible schemas fail registration;
legacy native-file migration is not supported on this host.

Authy's current Workers composition routes one configured identity realm to one
object, keeping credential uniqueness and session revocation in the same authority
domain. This is a bounded reference composition, not a sharding design for all Snap
applications. Transactions across objects are not supported by this Store adapter.

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
