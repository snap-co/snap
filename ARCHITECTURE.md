# Architecture

Snap separates capability contracts from their providers. Applications select
implementations and explicitly publish operations. Hosts own execution and IO.

## Active slice: Testy and standalone transport

`crates/transport` is the standalone `snap-transport` capability. It owns operation
contracts, admission, client correlation and resumable logical connections, and
compiles with `no_std` plus `alloc`. It has no dependencies on the older runtime,
Identity, Passport, Store or Cache. Testy is Snap's permanent contract application;
its portable server and SDK depend only on transport and serialization utilities.
The older integrations below have not been migrated and are outside this stage.

Applications own entry points and platform composition. `apps/testy/local` builds
the memory program, native server and native SDK program. `platforms/local` supplies
a local platform that mounts transport and selects memory or native IO. Native IO
is feature-selected; memory builds do not compile Tokio. The native-only executable
does not select the memory executor. A platform is not owned by transport, and
adding another capability later must not make it a transport dependency.

The single authority callback exchanges an opaque bearer for an identity string.
No session ID or lease enters transport. Non-connection requests resolve each
bearer; connected operations use the identity established at attachment. Reconnect
resolves credentials again, allowing rotated tokens that resolve to the same
identity. Wire callers cannot supply the server's attachment handle or identity.

Logical connections are keyed by verified identity and client ID. Attachment is
exclusive: an occupied connection rejects a contender without displacing its owner.
Unexpected disconnect retains application state for five minutes by default,
configurable by composition. Explicit close or detached expiry drops that state.
Platforms drive the monotonic timer even without new requests. Reattachment before
expiry restores state, but uses a new internal generation to fence old socket events.
Connection-owned reference counts release resident application objects when their
last owner leaves. Process shutdown loses all resident state; this is not persistence.

Operation contracts include input, output and declared application-error validators,
an identity requirement and a guard. Transport checks schemas and identity before
the guard, queues acceptance before entering the handler, and validates completion.
An invalid handler result is a contract failure, not a rollback of handler effects.
Per-attachment increasing invocation IDs reject duplicates; no automatic replay or
cross-reconnect result recovery is promised. IO failure leaves mutation outcome
unknown. Client code must not reuse an interrupted native exchange stream.

This first calculator slice uses synchronous authority, guards and handlers. Memory
delivery and native IO are asynchronous, and acceptance is locally queued before
handler entry. It does not yet supply suspended operation continuations. The native
adapter uses bounded length-prefixed JSON over TCP, with one invocation at a time
per physical connection. It is a local fixture, not a TLS or WebSocket deployment.
Workers/WebSocket compositions and migration of the old adapters are subsequent
stages, not dependencies of the selected build.

`calc.start` is Testy's fixture bootstrap, not a transport-reserved operation. The
SDK calls it anonymously for the constant bearer, connects with a client ID, then
calls it on the connection to allocate the calculator. Existing calculators are
preserved. An uninitialized connection can exist if bootstrap is interrupted.
Calculator arithmetic is checked signed-64-bit arithmetic; division truncates
toward zero. Failed calculations leave accumulator/history untouched. History is
bounded to 128 successful operations for this fixture. Different logical
connections own different calculators, even under the same identity.

## Earlier integration architecture

The following records the existing Authy/Chatty/Healthy implementation. It is not
the dependency or connection model of the active Testy build.

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

### Transport admission

The shared transport lifecycle is validation, guards, acceptance, handler
execution, then completion. Each operation defines an input schema. Transport
validates the payload against that schema and runs the operation's guards before
accepting it. Guards may inspect validated input and request identity or enforce
rate limits and quotas. A rejected invocation does not enter the handler.

Acceptance means validation and admission guards passed and the server owns the
work under its execution-lifetime contract. Transport emits `transport.ack` before
handing the invocation to the handler. This is a local ordering guarantee, not a
requirement that the remote client confirm receipt. Lost acknowledgement delivery
does not undo acceptance. `transport.complete` reports the handler's result or
error, including completion without a result. Acceptance does not promise success
or durable execution across a crash.

`snap_protocol::dispatch` owns operation lookup, schema validation, guard orchestration,
acceptance, correlation, and completion semantics. Hosts execute IO and poll work;
carrier adapters own physical encoding and delivery. An asynchronous memory
adapter passes invocation and event values without a serialization round trip,
while retaining the same client/server transport logic. Application scenarios
should run across memory and network adapters without changing their assertions;
wire-format and physical-host guarantees have separate tests. Guards establish
admission, while handlers retain transaction guards needed to keep authority and
data invariants valid when committing work.

`Operation` declares a key, input validator and identity policy. Validators are
executable schemas in this spike, not a schema-description language. Additional
operation-selected guards return owned asynchronous work and reservation leases.
Dispatch polls guards in order and retains leases through execution; rejection or
shutdown drops them. Guard implementations must defer effects until polled and
own reservation cleanup. Irreversible guard effects are not rolled back.

`Provider::prepare` is the trusted composition hook for resolving authority and
constructing typed handler input; carriers call `dispatch`, not this hook directly.
Passport resolves live session authority and applies the declared identity policy
here. `Accepted::start` emits acceptance before entering even a synchronous handler.
Admission refusals can retain capability-owned reply effects, such as clearing a
stale session credential, without entering the handler. Composition projects both
accepted results and refusals through the same reply adapter.
Native queues that signal to its socket observer; Workers sends it through its
socket adapter. Existing HTTP request/response bindings expose completion only.
An observer deadline ends delivery for that invocation, including any later
acknowledgement; it does not cancel independently owned admission/execution work.
The existing Authy HTTP/WebSocket binding choices remain compatibility policy;
automatic routing from identity requirements is not implemented in this slice.

`platforms/memory` supplies an explicit local executor, value-level delivery,
virtual time, transactional memory Store and an Identity SDK adapter. Its test
crypto is deliberately insecure and belongs only in isolated test compositions.
The native memory Store delegates to the same database implementation, so Store
contracts cover the backend used by the rig. Memory snapshots copy database state;
they do not snapshot live futures, clocks, tokens held by clients, or crypto counters.
Native/Workers retain physical attachment and receive-sequence management. A raw
TCP adapter, generic delegated authority, and distributed simulation are future work.

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

Dispatch creates owned admission work, then returns an accepted handler factory
with capability-specific context and output. The host emits acceptance, starts and
polls that handler, and performs external work. `Provider::invoke` is a convenience
for in-process consumers that do not observe acknowledgement. Immediate providers need not
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
