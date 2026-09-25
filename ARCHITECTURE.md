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
- `apps/testy` defines the calculator, operation contracts and SDK.
- `apps/testy/local` owns the executable entry points and selects the platform,
  authority, application implementation and host input resolver.
- `apps/testy/wasm` binds the same Rust SDK to browser-owned WebSocket IO.
- `apps/testy/web` owns the launcher, Healthy and calculator screens, and the
  development execution desk. Routing into `/calc` bootstraps the SDK.

Transport and execution do not depend on each other or on the earlier runtime,
Identity, Passport, Store or Cache. Testy selects both capabilities. Native IO is
feature-selected; memory builds do not compile Tokio, and native-only builds do
not select the memory executor. A platform is not owned by transport. Adding a
capability must not make it an unconditional dependency of other capabilities.

The earlier Authy/Chatty implementation and `tests/fixtures/healthy` use a different execution model.
Its reference is [legacy architecture](docs/legacy-architecture.md).

## Transport and connection lifetime

One configured authority exchanges opaque bearers for identity strings. No session
ID or lease enters transport. Non-connection requests resolve each bearer;
connected operations use the identity established at attachment. Reconnect resolves
credentials again, allowing rotated tokens that resolve to the same identity.
Wire callers cannot supply authoritative identity or server attachment handles.

Logical connections are keyed by verified identity and client ID. An occupied
connection rejects a contender without displacing its owner. Unexpected disconnect
retains logical state for five minutes by default, configurable by composition.
Reattachment before expiry restores state with a new generation that fences stale
socket events. Platforms drive the monotonic expiry timer even without traffic.

Transport holds stable logical connection IDs; the platform maps them to
execution-owned state scopes. Explicit close and detached expiry revoke new
dispatch immediately, then queue state release behind owned operations through
the execution gate. Process shutdown loses all resident state.

Increasing invocation IDs reject duplicates per attachment. Transport does not
replay commands after IO failure or provide cross-reconnect result recovery. Its
client must replace an interrupted native stream. Dropping a memory client future
after submission discards observation interest, not host-owned work.

The native adapter uses bounded length-prefixed JSON over TCP. The web development
host carries the same commands and observations as JSON text over WebSocket. Both
permit one outstanding command per physical connection. WebSocket messages are
bounded to 64 KiB. Browser IO retains frame text until Rust decodes it, preserving
64-bit integers. Rust SDK results cross the UI binding as decimal strings.
Workers, TLS deployment and migration of the earlier hosts are subsequent work.

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

## Testy calculator

The launcher links to `/healthy` and `/calc`. `health.up` is an anonymous stateless
application operation returning `{"status":"OK"}`. Both screens use the Rust SDK
and the same WebSocket host. A per-tab client ID survives reload in session storage.

`calc.start` is an application operation. The SDK calls it anonymously for a
constant fixture bearer, connects with a client ID, then calls it on the connection
to initialize the calculator. Existing calculators are preserved. Interrupted
bootstrap may leave an uninitialized connection. Different logical connections
have separate calculators even under the same identity.

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

The web host exposes these controls at `/__dev` independently of the application
gate, so tools can inspect and resume held requests. The UI uses that same HTTP
interface. Snapshots, restoration and program selection retain their idle-gate
requirements. The trace and delivered client results are not rewound. Replaying
uses a fresh invocation ID after restoring records; it is not transport redelivery.
`standard` and `double-add` are compiled variants, not dynamic code loading.

The host binds loopback only and rejects mismatched Host and browser Origin headers
on transport/control routes. These are local development controls, with full access
to fixture state. HTTP-owned tool peers require explicit drop; WebSocket peers
detach when the socket closes. The host ticks connection expiry even while held.
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
