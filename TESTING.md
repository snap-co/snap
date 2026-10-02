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
`no_std` with `alloc`; host adapters live in its native integration target.
The native target runs shared Store cases against the reusable controlled memory
backend, ephemeral SQLite and file-backed SQLite. SQLite in memory is still the
SQLite driver, not the controlled memory driver.

Transport cases use a duplex observation interface plus separate client/server
controls. Native adapters currently exercise TCP/TLS and JSON WebSocket servers,
and the production TCP client with a raw peer. Host queues supply output frames
at the carrier seam; they do not simulate module results. Cases cover command and
output delivery, malformed input, physical loss and the final-output retirement
regression where applicable to the selected role.

Store and carrier cases are independently selectable. Passing both does not yet
prove Transport-to-Store execution through a composed host or cartridge. Wasm,
memory Transport, browser client carriers and client-side durable module recovery
are not covered by this consumer. SQLite reopening/locking, migrations, process
crashes and TLS verification retain their distinct adapter-specific cases.

The package is a workspace default member, so its native cases run without ignore
flags under ordinary Cargo testing. `snap test <app> full` still selects app
declarations; it is not the route for these Snap-owned platform contracts.

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
apps own their server-specific composition and policy. Use the real module and
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

Keep end-to-end coverage when it protects a distinct composition risk, such as
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
| Snap property consumers | `tests/properties/Cargo.toml` selects cases beside their owning modules; it is a compilation/execution consumer, not a second owner of their contracts |
| Controlled execution and cartridges | `apps/testy/server/src/memory.rs` and `apps/testy/tests/` contain current examples; their location does not make app-owned copies of platform conformance the target design |
| Snap browser and React adapters | Fixtures in `kits/browser/tests/` and `kits/react/tests/`; Rust assertions in `tests/browser/src/client.rs` and `kits/react/tests/router.rs` |
| App SDK scenarios and properties | `apps/*/tests/` and app-owned `apps/*/properties/Cargo.toml` consumers |
| App end-to-end scenarios | `apps/*/tests/browser/journeys.rs`, executed by the shared Rust/CDP runner in `tests/browser/` |
| Snap tooling | `tools/cli/tests/` and `tests/cli/browser/journeys.rs` protect the real CLI and development workflow, not app domain behavior |
| Shared browser fixture support | `tests/browser/src/support.rs` owns processes, source copies and bundle hosting; app host composition is in `tests/browser/src/hosts.rs` |

Read manifests, suite declarations and runner code to determine actual selection.
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
