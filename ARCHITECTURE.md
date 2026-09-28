# Architecture

Snap separates portable capabilities from their host adapters. Applications select
implementations, explicitly publish operations and own their entry points. Hosts
own execution, clocks, randomness, external IO and task lifetime.

Keep portable contracts and behavior `no_std` with `alloc`. Contract consumers must
not depend on providers. Use modules to organize behavior; extract a crate when an
independent consumer or enforceable dependency/portability rule requires it.

## Package ownership

- `crates/transport` owns verified connection context, client correlation,
  envelopes and resumable logical connections.
- `crates/execution` owns the application interface, admission, private attempts,
  serialized host execution and in-memory commit. `src/program.rs` defines the
  application interface; `src/executor.rs` implements the host state machine.
- `platforms/local` composes transport and execution with memory or native IO.
- `crates/store` owns portable server-side resident transactions,
  index knowledge, miss diagnostics and explicit schema migration declarations.
- `crates/access` owns resource registration, direct grants, parent links and
  authorization evaluation in the caller's Store transaction. It has no host IO
  or dependency on Document.
- `crates/document` owns whole-document definitions, deterministic mutations,
  guarded Store writes, receipts, intent replication and the optimistic client SDK.
- `platforms/document` composes Document, Store and transport with a globally
  serialized FIFO and a local WebSocket carrier. Testy's ephemeral Executor host
  remains its separate application-selected composition.
- `platforms/sqlite` implements Store's host IO and database-backed durability.
  It is separately consumable by the CLI without importing application execution.
- `crates/identity` owns credentials, sessions and their operation dispatch using
  transport values and the caller's Store transaction. `platforms/crypto` supplies
  native cryptography; Testy's local composition connects Identity to transport.
- `crates/oidc` owns issuer protocol state, code/refresh lineage, consent and logout
  through caller-owned Store transactions. Its host supplies randomness, digests,
  signing and session authority.
- `crates/oidc::relying_party` owns private local OAuth sessions and continuation
  fences. `platforms/oauth` supplies native code/refresh HTTP, signed browser
  cookies and pinned RS256 verification. Applications compose its migrations with
  their own Store and use the Document host's serialized transaction gate.
- `apps/authy` composes atomic enrollment and Access-owned profile Documents.
  `apps/authy/native` owns HTTP, signed cookies, persisted RS256 keys and the
  serialized Document host. `apps/authy/wasm` binds the Document SDK for React.
- `apps/chatty` owns private conversation Documents and named message mutations.
  Its native host registers guarded WebSocket operations with the shared dispatcher.
  Chatty synchronizes client data and performs no model or tool IO.
- `apps/factorio` owns the shared workspace Document, ticket graphs, exclusive module
  claims and candidate/approval lifecycle. Its native host journals effect intent
  before Git or OpenCode IO and reconciles the exact planned integration commit
  after restart. Cookie-authenticated human approval is separate from agent-token
  commands. CLI and browser share the TypeScript carrier; Rust owns domain rules.
  Conversational intake uses the existing OpenCode V2 service through its official
  client. OpenCode owns execution and history; Factorio keeps intake metadata and
  validates scoped, revision-guarded draft writes in Store. Native SSE projects
  owner-authorized conversation snapshots after OpenCode events and reconnects.
- `crates/http` declares bounded outbound IO. `platforms/model` builds and consumes
  Responses streams through that contract. Neither is a portable application
  executor; applications must explicitly select these adapters when needed.
- `apps/testy` defines the calculator, operation contracts and SDK.
- `apps/testy/local` owns the executable entry points and selects the platform,
  authority, application implementation and host input resolver.
- `apps/testy/wasm` binds the same Rust SDK to browser-owned WebSocket IO.
- `apps/testy/web` owns the launcher, Healthy and calculator screens, and the
  development execution desk. Routing into `/calc` bootstraps the SDK.

Transport and execution do not depend on each other or on Store. Testy selects
transport and execution. Native IO is
feature-selected; memory builds do not compile Tokio, and native-only builds do
not select the memory executor. A platform is not owned by transport. Adding a
capability must not make it an unconditional dependency of other capabilities.

The supported applications use the current Store and transport. Authy and Chatty
have native hosts; their superseded Workers compositions have been removed.

## Transport and connection lifetime

One configured authority exchanges opaque bearers for identity strings or explicit
errors. No session ID or lease enters transport. Non-connection requests resolve
each bearer. Hosts may select live authority validation, as Testy does: transport
retains an opaque bearer and revalidates it for connected invocations and ticks.
Any failed validation retires that connection lifetime without mutating its persisted
session. Connect/request calls preserve authority errors such as Store misses.
Reconnect resolves credentials again, allowing rotated tokens that resolve to the same identity.
Wire callers cannot supply authoritative identity or server attachment handles.

Logical connections are keyed by verified identity and client ID. An occupied
connection rejects a contender without displacing its owner. Unexpected disconnect
retains logical state for five minutes by default, configurable by composition.
Testy's authenticated calculator selects zero retention.
Reattachment before expiry restores state with a new generation that fences stale
socket events. Platforms drive the monotonic expiry timer even without traffic.

Transport holds stable logical connection IDs; the platform maps them to
execution-owned state scopes. Explicit close and detached expiry revoke new
dispatch immediately, then queue state release behind owned operations through
the execution gate. Process shutdown loses all resident state.
Ephemeral composition instead calls `Executor::discard`: state disappears immediately,
queued and active calls fail, and late input cannot recreate the scope. Testy selects
this policy for disconnect, revocation, expiry and authority failure. The local host
revalidates authority before each executor step, including after held dependencies.

Increasing invocation IDs reject duplicates per attachment. Transport itself does not
replay commands after IO failure or provide cross-reconnect result recovery. Document
adds transactionally persisted receipts with IDs scoped to a logical connection. Its
wire driver assigns separate increasing physical invocation IDs. The exchange-style
transport
client must replace an interrupted native stream. Dropping a memory client future
after submission discards observation interest, not host-owned work.

The native adapter uses bounded length-prefixed JSON over TCP. The web development
host carries the same commands and observations as JSON text over WebSocket. Both
permit one outstanding exchange-style command per physical connection in Testy.
The Document host receives pipelined invocations but admits only one at a time.
It publishes capability pushes using `Response::Notification`. WebSocket messages are
bounded to 64 KiB. Browser IO retains frame text until Rust decodes it, preserving
64-bit integers. Rust SDK results cross the UI binding as decimal strings.
Development controls also use the Rust binding to validate supplied JSON text and
format inspection records/trace without passing integers through JavaScript numbers.
Workers and TLS deployment are subsequent work.

## Application interface and global gate

`Program::admit` returns Ready, Need, or Reject. `Program::attempt` returns Need,
Commit with proposed state and result, or Fail. Both entry points are synchronous,
IO-free and retain no invocation state in the program. The static build uses
ordinary Rust calls; it is not an IO sandbox or a stable binary ABI. A future Wasm
adapter will translate explicit data rather than pass Rust trait objects.

`Executor` permits one active operation for the whole application instance. The
gate covers admission through commit/failure, including dependency waits and
retries. All other operations stay in the FIFO. There are no per-object locks or
concurrent application dispatch. Hosts can continue network IO. Slow or unresolved
inputs deliberately stall dispatch; the host must resolve a read or supply failure.
Memory supports explicitly held reads. The native fixture selects an immediate,
nonblocking input resolver.

Operation descriptions declare input/output/error validators and an identity
policy. Execution checks schema and transport-verified identity before calling
admission. Admission may request read-only inputs before deciding. Ready produces
one Accepted observation; the next step enters the handler. The platform queues
the acknowledgement before that step. Acceptance promises host ownership, not
success, remote receipt or durability across a crash.

Need during execution discards all edits and restarts the handler with the same
invocation and accumulated inputs. The gate keeps admission state valid throughout
retries. No second acknowledgement is emitted. Requesting an already supplied key
fails; discovery is bounded to 64 distinct input keys per operation. These internal
pre-commit attempts are distinct from client retries after an unknown IO outcome.

## Working state and commit

Each logical connection currently has one data-only record represented by an owned
`serde_json::Value`. Request-local calls start with null and discard proposed state
after completion. Each attempt receives a deep copy of committed state. Reads see
its earlier edits. Input lookup consults a supplied map without host callbacks.
App-local allocations are dropped on return. There is no custom arena allocator or
binary state layout yet; memory delivery passes values without byte serialization.

Execution validates both the output and proposed state before publication. Failure
discards the whole proposal, including history edits. Success replaces the scoped
record before emitting completion. No other operation can see a partial update.
This is in-memory atomic publication, not a database transaction. Dependency
requests are reads, never irreversible effects. External write reconciliation and
multiple-object memory graphs are not implemented.

## Resident Store and durability

Store is a server-side Tier 0 capability. It has no dependency on transport,
execution, Identity, Access, Document or any host. Its interface is `snap_store`.
Cache expiry/eviction, authn,
authz, OLAP and distributed commit are outside this capability.

Portable module code receives one `snap_store::Transaction` shared across its module
calls. Reads are synchronous and access resident data only. A read returns rows,
known absence, or `Error::Miss` with the lookup. A miss poisons the transaction even
if application code catches the error. The attempt returns and all staged writes
are discarded. This is NOT `execution::Need`: there is no suspended invocation,
automatic loading, or automatic retry. A separate host action can `Store::load` a
table; a later explicit invocation may succeed. Miss diagnostics retain a lifetime
count and the latest 128 operation/lookup records, exportable by the host.

Hosts may load complete tables or explicit primary-key sets. Before loading, a Store knows only its own
committed inserts/deletes, not the rest of that table. Exact primary-key reads can
hit known records. Secondary-index or prefix reads require complete residency, so
a partially loaded set cannot be mistaken for a complete empty result. Loading an
empty table establishes known nonexistence. `retain_keys` releases unreferenced
rows and invalidates complete-index knowledge without deleting persistent data.
Indexes support ordered prefix lookups, including composite keys, with primary-key
tie breaking. Values are non-null text, signed 64-bit integers, or bytes.

`Store::run` holds an exclusive mutable borrow across the whole operation, database
commit and resident publication. An operation owns a deep-cloned scratch record
set and indexes. This intentionally favors simple isolation over memory efficiency;
it is not an arena allocator. Read-your-writes works across modules. Complete
inserts do not require a resident read; updates/deletes that depend on existing
rows do. All write statements commit together in one backend transaction.
Constraints can reject commit even after the handler returns successfully.

SQLite is the durability authority in phase one. Its adapter holds an exclusive
SQLite lock for its entire lifetime, uses rollback journaling with synchronous
EXTRA, and refuses concurrent owners or migrations. It validates unique and foreign
keys, deferring FK checks until transaction commit. No external writer may bypass
Store. In-memory SQLite is explicitly ephemeral and only used for experiments/tests.

A successful `Committed<T>` is returned after durable commit and publication of
the complete staged resident state. All allocation/index construction for that
publication precedes disk commit. No reader can enter between disk commit and
publication. A confirmed backend rejection preserves old memory. An indeterminate
commit fences the Store; reads, writes and loads fail until it is reopened and
recovered. An unknown outcome must not trigger a blind retry of a non-idempotent
operation. Restart starts cold and loads SQLite's committed state.

External services do not participate in Store's transaction. `Committed::changes`
exposes net before/after rows only after successful publication. The Document host
uses this feed to schedule controllers by Document type. Notifications are
in-process; startup scans recover missed notifications by reconciling stored
desired state against actual resources. Store does not claim exactly-once external
effects or client-request deduplication.

Migrations are explicit, ordered portable declarations translated to SQLite DDL.
The CLI creates templates and applies a pending batch atomically with its history.
Applied definitions cannot be changed, removed or reordered. Startup verifies the
recorded DDL shape; it never guesses migrations or adopts an unmanaged database.
Migrations run with Store closed. See [Store usage](docs/store.md).

The later effect-WAL/ring-buffer phase can change the backend durability authority
while retaining the transaction contract. No custom WAL, log shipping, consensus,
multi-writer execution or online schema change is implemented in phase one.

## Access and Document

Access composes with other modules through one caller-owned Store transaction.
Resource, grant and link changes become visible only after that transaction commits.
Hosts derive delivery invalidations from committed changes; a rejected attempt must
not publish invalidations or application effects. Store residency misses abort the
attempt and never trigger an implicit load or retry.

Graph evaluation favors correctness over caching. Direct and inherited grants combine
using the strongest role. Audience-derived viewing is separate from grant authority,
so public readability cannot authorize ownership or link edits. Authority checks use
the pre-change state of each Access operation. Applications must not expose raw Store
writes as an alternate path around these checks.

Access is implemented over Store. Document's initial client
loading policy is to load every document the client is authorized to read, including
its complete contents. Access supplies the authorized set; Document reconciles that
set with the client's holdings. Gaining access adds desired state; losing access
removes it from desired holdings and prevents further authorized delivery or writes.
Client removal cannot retract data already received.

Subset loading and sparse extents are deferred until real application use cases
justify them. The initial port does not introduce client interest filters or a
separate root-versus-extent loading policy. This client loading policy does not
change Store's server-residency contract: misses still terminate the transaction,
and hosts explicitly load server data.

Document's client and server should share the canonical in-memory representation
and deterministic mutation behavior as closely as possible. The server holds data
for many identities; a client holds its authorized set. Connection-owned references
are intended to retain server document data, while the client holds its own
references. The Document host retains server references across physical detach and
releases them only after logical closure and accepted-work drain. Its residency
loop loads the union of logical identities' requirements. A separate memory-only
loop compares each client's manifest with authorized resident data.

Clients keep a separate optimistic layer over authoritative state. Other replicas
receive ordered mutation intents; the originating client receives a completion
containing authoritative effects instead of replication of its own mutation. The
client applies that completion and removes the corresponding optimistic write,
preserving any other pending work. Shared representation is a design goal, not a
promise of identical native and Wasm memory layouts.

The optimistic layer is an ordered journal of pending mutation intents. The client
visible view is authoritative state with that journal replayed over it. A local
journal append, authoritative completion or incoming replication recomputes the
view and notifies reactive consumers such as React. Applying completion effects
and removing the completed journal entry publish one coherent view, without an
intermediate double application. Remaining entries replay in order.

Initially the journal is in memory and can hold ordinary named mutations and their
arguments. Future intent-replication bytecode and a persistent client journal should
fit this same model, supporting offline edits without changing how consumers read
the projected view. Bytecode, persistence and replay optimization are not designed
yet. Entries clear on authoritative desired-commit notification or completion,
not acceptance ACK; a rejection
must also be surfaced and reconciled with remaining optimistic work. Future offline
retention will require revisiting the initial connection-expiry reset policy.

The Document SDK applies local mutations optimistically and may send another
request after ACK. On the server, shape validation precedes protected admission.
The application gate covers authentication, Access guards, ACK, handler execution,
atomic desired-state commit, synchronous controller IO and finalization. Later
requests wait before ACK. Accepted authority survives expiry and revocation through
completion; permission mutations wait behind accepted work. ACK promises authorized
acceptance, not durability or success. Completion closes the invocation channel.

Controllers register by Document type. They inspect current conditions, perform
host IO, and commit meaningful observed changes through `ControllerContext` under
the same gate. Failed reconciliation preserves desired state and records a durable
blocked reason in Document lifecycle state. Automatic retries are deferred; an
explicit `document.retry` mutation clears the failure. Startup recovery skips
blocked Documents. There is no controller dependency graph.

`Event::Progress` shares the invocation ID with ACK and completion. SDK operation
contracts declare separate input, output, error and progress types. Progress is
informational and non-durable. The carrier drains output independently of the
execution mutex, so synchronous host IO cannot prevent ACK/progress delivery.
An internal Document `Committed` notification retires the optimistic mutation
before controller status updates arrive. It does not complete the invocation;
the final correlated completion still waits for controller IO.
Same-ID retries on a retained logical connection reuse acceptance/results and
reject conflicting inputs. Wire IDs remain unique across physical reconnections;
Document intent IDs additionally identify transactional receipts.

Physical detach does not release logical residency. Desired logical close prevents
new admission immediately and enters draining while accepted work remains. Actual
closure releases the connection's references after that work completes.
Carrier close/detach signals do not acquire the execution mutex. The socket drops
immediately; the host consumes signals before subsequent admission and at finalization.

Intent replay requires a matching authoritative base and compatible mutation
behavior. A replay mismatch is an explicit observable replication error. The SDK
pauses submissions and requests authoritative replacement through a manifest.

Mutation recovery is scoped to a surviving logical connection. Mutation receipts
commit atomically with their writes so an interrupted call can recover its
result without executing twice. Expiry of the logical connection clears client
document and optimistic state and starts a fresh manifest exchange; pending writes
from the expired lifetime are not replayed. This reset does not undo server commits.
Document's manifest protocol supplies recovery; generic transport alone does not.
The host prefixes receipt lifetimes with a fresh random boot namespace, so a process
restart cannot reuse an earlier logical connection's receipt IDs.

The manifest presents holdings and unresolved intents. The server returns missing
or stale snapshots, validated unchanged holdings, and matching receipts. Documents
absent from both snapshot and unchanged sets must be removed. It retains no
replication history. Recovered receipts remove journal entries without replacing newer manifest
snapshots with older completion snapshots. Reconciliation exposes each recovered
completion, rejection or forbidden result. A later journal replay failure is
reported alongside those outcomes rather than hiding them. Client state is ephemeral; a persistent
journal is future work.

Compatible mutations apply to the latest state in server FIFO order. There is no
blanket stale-base rejection. Applications can declare mutation-specific guards.
Replication carries the verified actor, base revision and canonical SHA-256 digests
of the base and result. A mismatch reports divergence and requests a manifest.
Ongoing Document synchronization follows current authorization and filters queued
pushes after permission changes. Invocation completions retain their accepted
authority. Bytes already handed to a socket cannot be retracted.

`document.mutate` invokes one named mutation on one document. Applications needing
atomic multi-document changes publish custom transport operations and compose the
document mutations through one caller-owned Store transaction. General optimistic
multi-document mutation is not part of `document.mutate`.

`Document::apply` composes a named mutation without creating a wire receipt.
Dispatch separates `admit` from `execute_recorded`, capturing authority before ACK
and committing the mutation and receipt together. Composite handlers share one
transaction and do not re-enter dispatch.

Document lifecycle metadata retains deleted/archived objects, finalizer keys and
blocked reconciliation status. Deletion and archiving remove Documents from normal
loading. Cleanup may remove finalizer keys but never physically purges the Document.
The Client SDK queues typed delete/archive/retry intents through normal receipt
recovery, leaving projected values unchanged until authoritative publication.
Direct retrieval, freezing and incineration are deferred. Legacy trusted replacement
helpers remain during application migration; they are not the public mutation API.

## Replacement and snapshots

Pausing stops new submissions while the host drains already queued operations.
Replacement requires an idle, paused gate, an equal application state version and
valid retained records. An incompatible replacement leaves the old program intact.
State has no application callbacks, vtables or destructors. Operation descriptions
are read from the selected program rather than cached across replacement.

The execution demo replaces a Rust implementation in process against retained
state. Module loading, Wasm memory layouts and unloading are later work. Statically
linked production builds use the same entry points without a loader.

Development snapshots copy idle state records and their version. Restore requires
a paused, idle gate and the same live scope IDs. It does not restore sockets,
clocks, pending IO or external side effects. Replay of a read-dependent invocation
must supply captured inputs separately.

## Identity and Testy calculator

The launcher links to `/healthy` and `/calc`. `health.up` is an anonymous stateless
application operation returning `{"status":"OK"}`. Both screens use the Rust SDK
and the same WebSocket host. Each physical calculator connection gets a fresh client ID.

`calc.start` requires an authenticated connection and initializes its calculator.
Identity enrollment and login issue durable sessions first. Reload or reconnect can
reuse a valid bearer but always gets a new calculator. Logout revokes one session,
discards every calculator attached through that session, and closes its sockets;
other login sessions remain valid. Browser tab session storage retains the bearer
for reload, but no calculator data. The UI clears calculator state on socket loss.

Identity operations use the local host's synchronous prepared-request dispatcher.
Portable parsing rejects malformed inputs before acceptance; the host queues an
Accepted observation before running the prepared closure under exclusion. Completion
follows Store commit. These operations remain usable while calculator execution is
held and never enter execution snapshots or traces. This is not a combined durable
commit for Executor state and Store: Identity commits Store, Calc commits ephemeral
memory. Callers doing persistent protected work must resolve session authority and
write through one Store transaction. See [Identity](docs/identity.md).

Arithmetic uses checked signed 64-bit integers; division truncates toward zero.
Failed calculations leave accumulator/history untouched. History holds at most
128 successful operations.

`calc.add_checked` writes its private accumulator and history before reading
`testy.calculator.ceiling`. A missing ceiling returns Need and discards those edits.
A supplied ceiling below the proposed accumulator fails with `AboveCeiling`.

## Development observation and control

`platforms/local::development::Development` owns stepping policy, response delivery,
a bounded observation trace and an idle-state snapshot. Its read-only inspection
reports committed records, the active operation, queued calls and requested inputs.
It never exposes references that can mutate live state. Manual mode stops host
stepping while continuing to admit submissions to the queue. The after-acceptance
breakpoint queues ACK and stops before the handler's first attempt. Each step
performs one executor observation; pending dependency reads require supply/failure.

The web host exposes a dedicated development WebSocket at `/__dev/ws`, separate
from application WebSockets at `/transport`. Both the execution desk and agents
can subscribe and issue correlated commands. Controls bypass the application gate,
so they remain responsive while application work is held or waiting. The existing
`GET/POST /__dev` supports one-off HTTP tool calls against the same host.

Each debugger receives a full inspection report on connection and pushed reports
when observable state changes, including changes from HTTP tools, application
requests and connection expiry. It does not poll. A host-wide revision orders
reports. A single latest-report slot coalesces changes for slow observers; it is
not an event delivery log. The existing bounded trace retains execution observations.
Socket writes occur outside the host lock with a five-second deadline. A debugger
cannot stall application execution through output backpressure.

Debugger connections allocate no application attachment. Disconnecting one leaves
application sessions and held work intact. Reconnection gets a fresh full report;
the desk rejects pending commands on connection loss and never replays them. Socket
commands execute in receive order, each with a correlated success/error response.
Command IDs identify responses, not durable deduplication keys. Snapshots,
restoration and program selection retain their idle-gate
requirements. The trace and delivered client results are not rewound. Replaying
uses a fresh invocation ID after restoring records; it is not transport redelivery.
`standard` and `double-add` are compiled variants, not dynamic code loading.

The host binds loopback only and rejects mismatched Host and browser Origin headers
on transport/control routes. These are local development controls, with full access
to fixture state. Explicitly opened tool peers require explicit drop, whether
created over HTTP or the debugger socket. Application WebSocket peers detach when
their socket closes. The host ticks connection expiry even while held.
See [development controls](docs/testy-development.md) for the executable interface.

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
