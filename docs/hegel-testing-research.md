# Hegel for Snap core testing

Evaluated on 2026-09-26, on branch `experiment/hegel-core-properties`.
The initial source review was followed by native Rust runs, mutation probes and
replay checks. This remains an opt-in experiment.

## Recommendation

Keep the experiment as an explicit core property suite. Transport's public
connection API works well with Hegel's state-machine runner. Execution and local
composition also expose the controls needed to check intermediate committed state.

The useful investment is a deterministic command runner and a contract model.
Keep those ordinary Rust so the choice of property-testing engine stays replaceable.

## Experiment results

The host-only `snap-core-properties` package registers 12 properties owned by
transport, execution and local composition. It pins Hegel 0.47.4 with
`static-engine`, so the engine participates in the workspace lockfile. Portable
production dependencies and default Cargo members do not include Hegel.

| Test target | Properties exercised |
| --- | --- |
| `transport-lifecycle` | Generated lease/ownership histories, token rotation, capacity, stale handles, exact expiry, monotonic time and saturation, repeated reconnect fencing |
| `transport-client` | Generated response grammar and correlation, lifecycle response kinds, unknown IO outcomes without automatic replay, IDs across channel replacement |
| `execution-properties` | FIFO batches across scopes, tentative balance/history writes, missing/failed/stale inputs, invalid proposals, admission reads, 64-input limit, scope release, snapshots and replacement, capacity and paused draining |
| `platform-properties` | Close/disconnect/expiry/reconnect while owned work waits, transport-to-execution scope lifetime, colliding per-peer invocation IDs, stateless requests |

The first seeded run passed 120,000 cases, 10,000 per property, in 20.687 seconds
including 0.35 seconds of incremental compilation. The transport state-machine
property permits up to 150 actions per history; these are case counts, not claims
that every case reaches the maximum length or represents a unique history.

The complete campaign passed 800,000 cases with no failure against unmodified
production code:

| Seed | Selection | Cases | Wall time including Cargo |
| --- | --- | ---: | ---: |
| `20260926` | All 12 properties | 120,000 | 20.687 s |
| `1` | All 12 properties | 120,000 | 20.581 s |
| `42` | All 12 properties | 120,000 | 20.717 s |
| `18446744073709551615` | All 12 properties | 120,000 | 20.157 s |
| `123456789` | Three transport lifecycle properties, 100,000 cases each | 300,000 | 134.243 s |
| `42` | Two composition properties after tightening reconnect assertions | 20,000 | Not separately timed |

Hegel's statistics reported these counts. Smaller development runs and intentional
mutation failures are excluded. This is evidence for these contracts and generated
histories, not a proof that transport or execution has no bugs.

Event statistics from that run confirm useful histories were exercised. For the
transport model, 31.4% of cases invoked a stale attachment and 8.8% resumed retained
connections. The client grammar saw malformed responses in 97.3% of cases and
successful correlated responses in 62.2%. Composition replaced retired scopes
while work was held in 84.3% of cases. These percentages overlap.

The initial dependency download/build plus three transport properties compiled in
5.21 seconds and executed in 0.29 seconds. Existing workspace artifacts were already
warm, so this is not a clean-machine build benchmark. A later warm run of all 12
properties, 200 cases each, took 0.545 seconds including Cargo. The existing
`bin/check` took 0.900 seconds. Both measurements ran while the longer experiment
was using the machine, and are observations rather than timing guarantees.

### Mutation and replay checks

Each probe temporarily changed production behavior, ran a property to failure,
and restored the original source. All five defects were detected and shrunk.
These were deliberately injected defects, not newly discovered Snap bugs.

| Temporary defect | Reduced failure | Property execution including shrinking |
| --- | --- | --- |
| Remove the attachment-generation fence | Connect, disconnect, reconnect, disconnect with the old handle; capacity 1, retention 0 | 0.96 s |
| Retire one tick after the deadline instead of at it | Retention 1, time at the expiry boundary, resident count disagrees | 0.04 s |
| Stop checking completion ID against acceptance ID | Accept ID 1, complete ID 0 with an application error | 0.10 s |
| Publish proposed state before validating output/state | One zero-delta operation with invalid output publishes a history entry | 0.30 s |
| Accept dependency replies for the wrong ticket | Two operations, second waiting for input, reply using the first ticket | 0.32 s |

Hegel's printed reproduction blobs replayed the generation-fence and premature
publication failures with the database disabled. Each replay completed in less
than Cargo's 0.01-second reported test-time resolution. The zero-retention
attachment history is also retained as an ordinary transport contract test, so it
does not depend on an engine-specific blob. All temporary production edits and
reproducer attributes were removed.

Source mutation and blob replay were tested; database persistence and real-thread
concurrent mode were not part of this experiment. Ordinary generation uses the
sequential engine and explicitly controlled events.

Commands, profiles and reproduction instructions live in
[TESTING.md](../TESTING.md#core-property-testing-experiment).

## What Hegel contributes

Hegel is a property-based testing engine built on Hypothesis, with a Rust library
and libraries for several other languages. Its Rust interface draws generated
values inside a `#[hegel::test]` function and uses ordinary assertions.
[Hegel introduction](https://hegel.dev/)

For Snap, the desired shape is:

```text
Hegel generates a command sequence and dependency outcomes
    -> a fresh test driver runs each command
    -> the real Snap implementation and independent model produce observations
    -> assertions compare those observations and check invariants
    -> a failure becomes a reduced, replayable command sequence
```

Snap supplies the model, event controls and assertions. Generating data alone does
not know what rollback, connection ownership or a valid commit means.

### Verified Rust capabilities and integration costs

The inspected source is commit `bf88919d3c865e83ca7aada13dc4c4ca22ba3d68`,
with frontend `hegeltest 0.47.4` and engine `hegeltest-c 0.43.7`.
The package is `hegeltest`; Rust imports use `hegel`. Both are MIT licensed.
Hegel explicitly calls itself beta and allows breaking minor releases. Pin the
evaluated version. [Manifest][h-manifest], [compatibility][h-compatibility]

- Ordinary `cargo test` runs `#[hegel::test]` tests. The frontend requires Rust
  1.86 or later and uses `std`, which is suitable for native tests of Snap's
  portable libraries. It does not establish support for running the frontend
  inside Snap's browser or bare-Wasm targets. [Manifest][h-manifest], [Rust API][h-api]
- Current Hegel runs in-process in Rust. Python, `uv` and a testing server are no
  longer required. The website's getting-started instructions still describe the
  older server design; use matching source/docs for setup. [Changelog][h-changelog],
  [getting started][h-start]
- By default, its build script invokes Cargo again to compile a shared engine.
  That nested workspace resolves dependencies separately and the invocation has
  no explicit `--locked` or `--offline`. Do not assume Snap's lockfile governs the
  complete engine build. The `static-engine` feature makes the engine a normal
  Rust dependency and removes shared-library discovery. I would evaluate this
  mode first for Snap's test runner. [Build script][h-build], [manifest][h-manifest]
- Generators support bounded collections, enums, recursive values, mapping and
  dependent draws. The Rust state-machine API provides rules and pools of created
  values, so Snap does not need to invent sequence generation from scratch.
  Keep the command application/model logic independent of those macros.
  [Generators][h-generators], [state machines][h-stateful]
- Use `#[invariant(always_run)]` for rollback and other intermediate-state checks.
  Plain `#[invariant]` is sampled between steps. Configure sequential histories
  with `hegel::stateful::machine(model).steps(n).run(tc)`.
  [State machines][h-stateful]
- Keep the state machine sequential and generate explicit scheduling decisions.
  When maximum concurrency exceeds one, Hegel disables shrinking, replay,
  persistence and reproduction blobs because thread scheduling is uncontrolled.
  That mode would sacrifice the main benefit for Snap's already-serialized core.
  [State machines][h-stateful]
- Failures can produce shrunk cases, persisted examples and
  `#[hegel::reproduce_failure("...")]` blobs. Blobs are version-specific; a seed
  alone is not a durable regression across changes to generators or test code.
  Preserve important domain command histories as ordinary Rust regressions.
  [Rust API][h-api], [settings][h-settings]
- Local examples default to `.hegel/examples`. The built-in CI profile instead
  uses deterministic seeds and disables the database. Configure persistence or
  retain reproduction output explicitly for automated exploration. The shrinking
  safety budget can reach 300 seconds, reinforcing the need for a separate gate
  while evaluating costs. [Settings][h-settings], [changelog][h-changelog]

## Where it fits in this repository

The active core is `snap-execution` and `snap-transport`, composed by
`snap-platform-local`. Store, Identity and the earlier runtime are separate,
older integrations. Do not pull those packages into Testy's core tests merely to
exercise them. [Local architecture](../ARCHITECTURE.md)

| Owner | Generated inputs and actions | Contract to check |
| --- | --- | --- |
| `crates/execution` | Submit, step, read success/failure, stale replies, scope release, pause/resume | Need and failure publish no edits; accepted work holds the global gate; queued work stays FIFO; stale input cannot resume another job; closing blocks new submissions while owned work can finish |
| `crates/execution` | Idle snapshots, compatible/incompatible replacements, restore attempts | Rejected replacement preserves program and state; restore requires a paused idle gate and compatible version/live scopes; replay under the same code and captured inputs reproduces application state/results |
| `crates/transport` | Connect, disconnect, reconnect, close, tick, duplicate invocation IDs, old attachment handles | Identity isolation; an occupied connection keeps its owner; old generations cannot invoke or close a replacement; expiry retires a connection once; invocation ordering is per attachment |
| `crates/transport` client | Generated response event lists and IDs | Incorrect correlation, duplicate acceptance and invalid completion order produce protocol errors |
| `platforms/local` | Interleave peer loss, virtual expiry, held reads and submitted work | Transport retirement revokes dispatch immediately while execution releases data only after owned work; observer loss does not cancel submitted work |
| `apps/testy/tests` | Arithmetic sequences, boundary integers, ceilings, failed operations | Checked arithmetic matches an independent numeric model; failed operations preserve accumulator/history; history stays within 128 entries; read retries do not duplicate history |
| Store contract and adapters | Small schemas, guarded transactions, reads/writes, constraint failures | Atomic rollback, reads of prior writes, declared ordering and constraint behavior match a reference model; run a selected corpus against each real adapter |

These are proposed properties derived from the public contracts, not findings of
bugs. Existing examples cover many individual cases; generation explores their
combinations. Sources: [execution interface](../crates/execution/src/program.rs),
[executor](../crates/execution/src/executor.rs),
[execution tests](../crates/execution/tests/execution.rs),
[transport server](../crates/transport/src/server.rs),
[client tests](../crates/transport/tests/client.rs),
[local composition](../platforms/local/src/lib.rs),
[memory delivery](../platforms/local/src/memory.rs),
[Testy program](../apps/testy/src/program.rs),
[Testy memory tests](../apps/testy/tests/memory.rs),
[Store contract](../crates/store/src/lib.rs) and
[existing Store adapter tests](../tests/store/contract.rs).

## Test design

Use a package-owned test program in `crates/execution/tests`, following the existing
execution fixture. Core tests must not depend on Testy. Start with two scopes,
small bounded arithmetic and short action sequences. Keep arithmetic overflow out
of the executor fixture unless overflow behavior is the property under test.

Generate actions such as:

```text
Open(scope)
Submit(scope, delta, dependency_keys, outcome_mode)
Step
Supply(current_or_old_ticket, correct_or_wrong_key, value_or_failure)
Release(scope)
Pause / Resume
```

The model tracks committed values, accepted work, outstanding reads and queued
operations. It must express the contract independently rather than copy the
executor's implementation. Compare public outcomes and committed state after each
action. Check acceptance counts and FIFO ordering where the interface promises
them, without asserting incidental internal steps.

Include both valid sequences and deliberately invalid actions with defined errors.
Choose usable handles from a test-owned table and retain old handles for stale-reply
cases. Give shrinking a defined interpretation for missing handles so deleting an
earlier action cannot turn a product failure into a test-driver panic. Recreate the
executor, model and handle table for every example.

One useful sequence is:

```text
submit A, whose attempt writes before requesting an input
step until A requests the input
submit B
release A's scope
send a reply for B against A's outstanding read
supply A's correct reply
drain execution
```

Committed state must stay unchanged during A's wait. The wrong reply must fail
without progress. Already owned work must complete before its scope is released.
New work on that closing scope must fail. Hegel should be able to reduce a failing
sequence to a smaller counterexample.

Bound the number of actions and drain steps. A held read is a valid waiting state,
not a timeout failure. Check eventual completion only after the driver supplies or
fails outstanding reads and permits the executor to run. Add snapshot/replacement
actions once the smaller model is useful.

Use explicit boundary cases alongside random generation: 64 versus 65 distinct
dependency keys, zero/full queue capacity, expiry just before/at/after its deadline,
and Testy's 128-entry history limit. Short random sequences alone are unlikely to
reach every limit. These boundaries come from the executor, transport and Testy
sources linked above.

The mutation probes above check that the properties detect broken behavior, shrink
it and replay it. Build, ordinary passing execution and shrinking costs are reported
separately.

## Rules for future core systems

For each capability, define the public commands, legal failures, observable state
and invariants alongside its interface. Build a tiny independent model, then let
generated sequences exercise the implementation. Keep clock advances, random
choices, dependency outcomes and delivery decisions under test control.

The current global execution gate makes this easier: scheduling tests choose the
order of explicit host events rather than arbitrary interleavings inside a handler.
Generated schedules only cover what the test driver exposes. They cannot establish
correctness for uncontrolled threads, network delivery or crash recovery.

For future identity/cache work, useful properties include revocation and expiry
remaining authoritative despite stale cache snapshots. Derive their precise rules
from the capability's contract when that flow is selected. Do not infer durability
or external-write semantics from the current in-memory executor.

Snapshot-based replay has a narrower promise than whole-system replay. Current
snapshots copy application records and their version, not clocks, sockets, pending
IO or dependency replies. Capture the generated command list and supplied inputs
separately. Compare application results/state, not newly allocated ticket IDs.
[Snapshot contract](../ARCHITECTURE.md#replacement-and-snapshots)

For failures reproduced with Testy, translate the reduced scenario into development
controls so a human can inspect the held operation and committed state. Keep the
primary regression as an in-process test. The existing execution desk exposes
stepping, input supply/failure, snapshots and compiled program selection, but it is
not yet an importer for arbitrary generated command traces.
[Development controls](testy-development.md)

## Test ownership and gates

Use native host tests against portable code. Keep generation libraries out of
production dependencies and preserve the `wasm32v1-none` compilation gate. Put
capability properties with their owning packages and composition properties in
`platforms/local`; keep product policy in application-owned tests.

Begin with an explicit property-test gate. The existing `bin/check` runs all selected
Cargo tests and must remain within single-digit seconds on a warm build. An ignored
test still compiles its test dependencies, so ignoring an expensive property is not
enough to isolate a costly build-time toolchain. Measure that cost before selecting
the final Cargo target/dependency arrangement.
An optional normal dependency behind a core feature would also be the wrong place
for a host-only testing engine: every core feature must remain portable under the
workspace's all-features check. [Dependency rules](../ARCHITECTURE.md#dependency-enforcement)

Preserve shrunk command traces as ordinary deterministic regressions. Keep broad
random exploration separate from real native/browser/Workers adapter checks.
Protocol round trips need independent wire examples too: encoder and decoder can
agree on the same mistake. These recommendations follow the current
[testing policy](../TESTING.md) and [default gate](../bin/check).

The normal gate, property-package Clippy and workspace structural/portable check
passed. The dependency change is confined to the explicit host test consumer and
its lockfile entries. The existing testing document describes its opt-in gate.

## Hegel primary sources

Version-sensitive links below point to the inspected commit, rather than moving
`main` documentation.

[h-manifest]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/Cargo.toml
[h-compatibility]: https://hegel.dev/compatibility
[h-api]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/src/lib.rs
[h-changelog]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/CHANGELOG.md
[h-start]: https://hegel.dev/intro/getting-started
[h-build]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/build.rs
[h-generators]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/src/generators/generators.rs
[h-stateful]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/src/stateful.rs
[h-settings]: https://github.com/hegeldev/hegel-rust/blob/bf88919d3c865e83ca7aada13dc4c4ca22ba3d68/src/docs/settings.md
