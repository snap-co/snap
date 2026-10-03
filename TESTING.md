# Testing

Read this before changing tests, harnesses, fixtures, test routing or testing
checks. These are the agreed testing seams and ownership rules. The common
platform suite described below is the target design, not an assertion that every
setup already plugs into one harness. Use [CONTEXT.md](CONTEXT.md) for domain terms.

## Platform conformance

Snap owns one reusable set of platform contract tests. A setup supplies an
execution environment, Transport and Store drivers, and an explicit client-side
or server-side Transport role. The cases and expected behavior stay shared;
adapters supply setup, external IO, observations and teardown.

Transport role and Store choice are independent. A client-side TCP setup can
have its own SQLite Store; a server-side setup can use memory drivers. Native
and Wasm are execution environments, not shorthand for a particular driver
combination. Exercise supported combinations through the same applicable cases,
rather than copying assertions into each driver or app.

The suite checks the guarantees of the selected interfaces, including delivery,
ordering, failure outcomes, connection lifetime and transaction behavior where
promised. Use a Snap-owned test cartridge when execution requires application
behavior. Apps need not implement platform conformance or know how its fixtures
work. Keep adapter-specific tests for distinct guarantees such as certificate
verification or crash durability that the shared cases cannot express.

Client and server roles have different applicable cases. A transport scenario
may need a peer supplied by the harness, but that peer must not manufacture the
result the implementation under test should produce. A setup must identify its
supported guarantees. Report unsupported cases explicitly; missing support and
zero selected cases are not successful verification.

Client-side persistence and catch-up after reconnect are intended capabilities,
not a ban on client Store use. Document's current client keeps snapshots and its
pending journal in memory and does not use Store. Test durable recovery when an
implementation promises it; do not infer it from the presence of a Store driver.
The setup model does not imply that every Wasm environment already has SQLite.

Snap also owns reusable testing platforms that control external inputs and
failures. Apps can select these to construct their own test environments without
reimplementing driver behavior. Faults belong at the dependency interface, not
in a replacement implementation of the module being tested.

### Current shared suite

`tests/platform/` is the first shared conformance consumer. Its library is
`no_std` with `alloc`; host adapters live in native integration targets and shared
support used by the native example runner.
The native target runs shared Store cases against the reusable controlled memory
backend, ephemeral SQLite and file-backed SQLite. SQLite in memory is still the
SQLite driver, not the controlled memory driver.

Transport cases use a duplex observation interface plus separate client/server
controls. Native adapters currently exercise TCP/TLS and JSON WebSocket servers,
and the production TCP client with a raw peer. Host queues supply output frames
at the carrier seam; they do not simulate module results. Cases cover command and
output delivery, malformed input, physical loss and the final-output retirement
regression where applicable to the selected role.

Store and carrier cases are independently selectable. The IO-free cartridge in
`tests/platform/src/cartridge.rs` also exercises actual Transport-to-Store
execution through Document's portable runtime and Transport's transactional
executor. Shared properties and a
scalar reference model live in `tests/platform/src/dispatch.rs`. The controlled
native server-role adapter drives public commands, output and execution steps
against memory, ephemeral SQLite and file-backed SQLite. It does not replace the
dispatcher or supply acceptance/completion events. Transport carries one event
per frame, so a `drain` is already flat and a fixture that wants a whole exchange
at once collects the frames. Cartridge tables start cold;
verification reads declare their data again so backend reloads can detect writes
that reached resident memory only.

Fixed examples run under ordinary Cargo tests. Hegel generates bounded batches,
retry histories and observer-loss histories through the `platform-dispatch`
target of `tests/properties/Cargo.toml`. That consumer uses the same cases and
adapters; it keeps Hegel out of the default-member build. The memory-only fault
setup rejects the next nonempty backend commit before writing. SQLite fault
injection and unknown-commit recovery are not covered by that setup.

The paired `cartridge_tcp` integration target additionally runs the portable
client journey in `tests/platform/src/journey.rs` through the real Transport
client SDK, production TCP/TLS driver and native dispatcher into file-backed
SQLite. It checks acceptance and completion against the same independent scalar
model, reads after each change or failure, then stops the server and reopens
SQLite to verify persisted rows. The `plumbing` Cargo example uses that same
setup and prints the exchange, retaining a fresh database for inspection. Native
assembly lives in `tests/platform/support/{host,tcp_sqlite}.rs`; it supplies no
operation results. Its deadline bounds connection and journey execution in wall
time, not virtual time; startup and reopening are synchronous. Setup regressions
check that existing database files, SQLite companion paths and dangling companion
links are rejected without changes, assuming no concurrent directory modification. The
cartridge identity is a fixed fixture, not an authentication-flow test. Reopening
after orderly teardown is not process-crash or power-loss durability proof.

The controlled command tests alone do not prove a socket-to-host path or
constitute a production memory Transport driver. Wasm execution, browser client
carriers, client-side durable module recovery and a full Transport/Store matrix
remain unsupported here. SQLite reopening/locking, migrations, process crashes
and TLS verification retain their distinct adapter-specific cases.

The package is a workspace default member, so its native cases run without ignore
flags under ordinary Cargo testing. `snap test <app> full` still selects app
declarations; it is not the route for these Snap-owned platform contracts.

### Transport properties

These rules describe interface promises, not every behavior of a network. Keep
the independent model reviewable: passing exploration means the implementation
matched the encoded rule for explored examples, not that the rule is correct or
that every input and schedule was checked.

| Rule | Primary proof |
| --- | --- |
| Trusted host resolves identity; stale attachments cannot acquire a replacement's authority. | Existing Transport lifetime model in `crates/transport/tests/properties/lifecycle.rs` |
| Hosts load declared data before admission. One FIFO owner holds the lane through admission, execution and publication; later guards see prior committed state. | Cartridge batch model, with cold tables and backend-loaded reads |
| Accepted means admission, not success. Application errors, invalid output and caught Store failures discard staged writes. Confirmed commit rejection must not become success or an automatic retry. | Cartridge batch model and the controlled commit-rejection case |
| Exact invocation retries observe the same result without another mutation within a retained logical lifetime. Different input under that key is Protocol. | Cartridge retry model, including pending retries and replay after a later mutation |
| Reconnect alone must not inject an old completion into a fresh call. Explicit retry reattaches observation interest. | Cartridge retry model |
| One invocation spans several frames. Acceptance reaches the client before the handler finishes, progress arrives while it still runs, and completion terminates it. A channel is a stream: sending never waits for a reply. | `crates/transport/tests/client.rs` routing cases and the `transport-client` property model |
| Correlation is judged per frame. A frame naming another invocation is dropped without touching this one; a frame that breaks this invocation's ordering contract abandons its trace and is reported immediately, not left to wait. | `transport-client` property model, which distinguishes a broken sequence from an unfinished one |
| Unhandled global pushes are dropped silently and never fail a client, because any server can publish a topic nobody subscribed to. | `crates/transport/tests/client.rs` |
| Observer loss does not cancel accepted work. Disconnect can retain retry state; Close and expiry end that scope after accepted work drains. | Cartridge draining model; core lifetime model separately checks retention boundaries and stale handles |
| Carriers deliver commands and published output independently of execution, including final output at retirement. Malformed input never enters dispatch. Physical IO loss is not a successful empty response. | Shared fixed carrier cases in `tests/platform/src/transport.rs` |

The cartridge defines a compare guard and two rows that must move together. Its
scalar oracle computes acceptance and committed values from inputs alone, never
from production responses. Generated histories vary batch sizes, amounts, stale
compares, handler outcomes, retries and observer loss. Authentication/credential
rotation, capacity, arbitrary wire corruption, unknown commit outcomes and
client correlation retain their existing focused tests rather than being
claimed as coverage of this cartridge. Retained invocation replay is not a
global exactly-once or crash-recovery guarantee.

## Client SDK

The client SDK is the primary domain-testing seam. Drive production module
interfaces as a client would: configure modules and document definitions, invoke
operations or mutations, and observe results and published state. UI kits,
agent workflows and other clients should consume this behavior rather than
implement it again.

Snap owns module tests across configurations, including different document
shapes, mutation patterns, permissions, reconciliation and connection lifetimes.
Apps own their definitions, module combinations and workflows at this same seam.
The driver setup is supplied by a platform harness; it is not part of the scenario.

Use focused examples, property/state-machine tests, goal-directed client journeys
and multi-client simulations according to the failure being exercised. A journey
is a module that drives the SDK from the top down. It need not have a browser or
a human UI. A host can drive a native or Wasm client until completion, an exit
condition or a wait for external input.

Seeded exploration should retain the seed, setup and failing action sequence.
Controlling action selection alone does not make real IO deterministic. Claim
deterministic reproduction only for the inputs and scheduling the testing
platform actually controls; strict deterministic simulation remains deferred.

## Direct server interfaces and controllers

Test a public server-side module interface directly when it has a contract that
client operation dispatch cannot exercise. Snap owns reusable module cases;
apps own their server-specific host and policy. Use the real module and
its transaction interface, with the authority required by that interface.

Direct calls do not prove dispatch admission, credential handling or wire
behavior. Those guarantees belong to their own seams. Conversely, avoid routing
every server-only behavior through a synthetic client operation just to test it.

Controller definitions and their technical place in the execution loop are
deferred. Preserve existing controller regressions without making their current
shape the required harness design.

## Client interfaces and end-to-end tests

Apps own their end-to-end cases and journeys. Drive the interface available to
the actual consumer, whether human, agent or another application. Focus on
accessible controls, navigation, rendered state, loading and error presentation,
and correct connection to the SDK. Domain mutation correctness primarily belongs
in SDK tests, not repeated across UI journeys.

Keep end-to-end coverage when it protects a distinct host risk, such as
real credential delivery, host startup, reload, binding disposal or account data
remaining visible after an identity change. Move an existing domain assertion
only after equivalent owner-seam coverage exists. A failing journey may expose a
product bug; it is not a reason to remove the journey.

Snap owns generic runners and platform-level fixture support that apps can use.
Snap also owns tests for its browser/Wasm and UI-kit adapters. These exercise
adapter-specific lifetime, publication and rendering guarantees without
retesting the underlying module's domain behavior. Runner location does not
transfer ownership of an app scenario to Snap.

## Where this lives in the codebase

| Owner / seam | Existing code |
| --- | --- |
| Snap platform conformance and controlled storage | Shared portable cases and memory backend in `tests/platform/src/`; native setup adapters and default-run coverage in `tests/platform/tests/` |
| Snap interfaces, modules and adapter-specific guarantees | `crates/*/tests/` and `crates/platform/*/tests/`; shared Store and carrier conformance lives in `tests/platform/` rather than provider-local copies |
| Identity credential flows and private session policy | `crates/identity/tests/{identity,oauth,operations}.rs`; native WebAuthn signatures, origin/counter policy and durable ceremony state in `crates/platform/identity-native/tests/passkey.rs`, selected by the `passkey` feature; Authy's `passkey browser ceremony` journey uses a CDP authenticator to cover browser/Wasm conversion and cookie delivery; OAuth refresh/socket integration remains app-owned in Factorio |
| Document runtime and native execution integration | Controlled document lifecycle and socket cases in `tests/platform/tests/document_{runtime,tcp}.rs`; generated document histories in `crates/document/tests/properties/runtime.rs` via the `document-runtime` property target; independent execution/output-lock regressions in `crates/platform/transport-native/src/dispatch.rs` |
| Snap property consumers | `tests/properties/Cargo.toml` selects cases beside their owning modules; it is a compilation/execution consumer, not a second owner of their contracts |
| Snap Transport-to-Store cartridge | Portable cartridge/model in `tests/platform/src/{cartridge,dispatch}.rs`; real-host setup in `tests/platform/tests/support/dispatch.rs`; fixed examples in `tests/platform/tests/dispatch.rs` and Hegel inputs in `tests/platform/tests/properties/dispatch.rs` |
| Paired cartridge client and physical IO | Portable SDK journey in `tests/platform/src/journey.rs`; shared native assembly in `tests/platform/support/{host,tcp_sqlite}.rs`; default-run integration in `tests/platform/tests/cartridge_tcp.rs` and visible runner in `tests/platform/examples/plumbing.rs` |
| App controlled execution | `apps/testy/server/src/memory.rs` and `apps/testy/tests/` contain app examples, not the owner of Snap platform conformance |
| Snap browser and React adapters | Fixtures in `kits/browser/tests/` and `kits/react/tests/`; Rust assertions in `tests/browser/src/client.rs` and `kits/react/tests/router.rs` |
| App SDK scenarios and properties | `apps/*/tests/` and app-owned `apps/*/properties/Cargo.toml` consumers |
| App end-to-end scenarios | `apps/*/tests/browser/journeys.rs`, executed by the shared Rust/CDP runner in `tests/browser/` |
| Snap tooling | `tools/cli/tests/` and `tests/cli/browser/journeys.rs` protect the real CLI and development workflow, not app domain behavior |
| Shared browser fixture support | `tests/browser/src/support.rs` owns processes, source copies and bundle hosting; app hosts are in `tests/browser/src/hosts.rs` |

Read manifests, suite declarations and runner code to determine actual selection.
Each module and host storage component owns one initial migration. Identity's
single schema supports password, OAuth and passkey flows; hosts select it without
an alternative OAuth history. The CLI migration gate applies Identity's directory
to a fresh database and verifies credential/session persistence after reopening.
File location, `cargo test` success and an app's `full` selector do not establish
repository-wide coverage. Keep fast controlled tests distinct from real-IO gates
without treating speed as a test's ownership or value.

## Changing tests and harnesses

Before adding or moving a case, identify its owner, production interface,
observable contract, credible regression and execution route. Explain the
additional risk if another test already covers the contract. Prefer extending
shared cases over provider-local or app-local copies.

Expected outcomes must come from an independent contract or model, not from the
implementation under test. Supply dependency inputs and faults, not receipts,
admission decisions or SDK publications the owner should produce. Exercise real
crypto, storage and wire behavior at the adapter seam when a controlled substitute
cannot prove the guarantee.

Keep test access on production interfaces. A test-only export, flag or wrapper
requires re-examining the chosen seam. Preserve meaningful security, protocol,
storage, configuration and regression tests; assertion counts and test/source
ratios are not reasons to add or delete coverage.

Route each runnable suite explicitly, including properties and real-IO gates.
Ignored cases need a reason and an intentional route, including subprocess helpers
invoked by another case. Report selected setups, unsupported cases and unavailable
checks honestly. Automatable checks should enforce ownership/dependency direction,
suite reachability and nonempty selection, not source-shaped assertions or blanket
coverage percentages. These rules are policy, not a claim that all checks exist.

When adding or changing a harness, update this document's seam and ownership map
in the same change. Keep command details in executable configuration and command
help rather than duplicating them here. Portable scenarios remain `no_std` with
`alloc` where applicable; host runners own execution and external IO.
