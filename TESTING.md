# Testing

## Fast iteration

`snap check` and `bin/check` must stay within single-digit seconds on a warm build,
ideally two to five. Measure cold compilation separately. Investigate individual
tests taking more than 100 ms, including setup and cleanup; this is a design signal,
not a timing assertion to put in a test.

The default gate runs source checks and fast behavior tests. Browser, real-network,
cross-process and property exploration suites run as explicit gates. A slow test
does not belong in the iteration loop merely because it covers important behavior.

## Ownership and interfaces

Assign each promise to the package that owns it. Transport, execution and Store
contracts live under their crates. Host adapters own real IO and durability tests.
Testy owns its application policy, composition and browser journeys under
`apps/testy/tests`. Root tests cover CLI tooling and the shared property runner.

Use Cargo's native runner for Rust behavior without launching the CLI, an app server
or a browser. Portable production code stays `no_std` with `alloc`; tests may use
host executors and dependencies. Choose the cheapest public interface that owns the
promise:

- In-process contracts exercise behavior with controlled clocks, state and failures.
- SDK tests exercise commands, observations, errors and lifetime. Shared cases need
  not run through every network adapter.
- Wire tests use independent encoding examples. Round trips alone can let an encoder
  and decoder agree on the same mistake.
- Adapter tests exercise real sockets, persistence and host compatibility.
- CLI/browser tests cover commands, rendering, packaging and process lifetime.

Keep one primary home for each promise. A small composition test checks wiring;
it does not repeat every dependency's contracts. Assert observable results rather
than private state, SQL text, helper calls or incidental event ordering.

## Journeys and fixtures

End-to-end journeys follow frequent user processes in their natural order. Let
failed operations, panics and timeouts fail the journey. An expected failure that
returns a value rather than throwing needs an explicit check. Keep detailed edge
cases in focused tests at their owning interface.

In-process fixtures run the real portable behavior with controlled time, randomness,
external results and scheduling. Simulated faults must respect dependency contracts.
Seeds and schedules should be reproducible. Test host obligations separately against
real adapters, sharing scenarios where the host-independent promise is the same.

Prefer in-process construction/reset. When reusing a live server, reset all relevant
state, including caches, clocks, pending work and observations. Transaction rollback
alone is sufficient only if all changes participate. Expensive fixtures may provide
isolated copies of a verified non-empty baseline; cases must not mutate a shared
baseline or depend on another test running first. Provisioning and migration tests
still exercise fresh setup.

Fixtures own temporary data and processes, use ephemeral ports and clean up on
failure. Synchronize on readiness/completion rather than fixed sleeps. Timeouts bound
stalled tests; virtual time exercises elapsed-time behavior where the interface allows.

## Current commands

The active Cargo workspace contains transport, execution, Store, their local/SQLite
adapters, Testy, the CLI and the property consumer. Authy, Chatty and HTTP/LLM sources
are excluded pending rewrites. Their tests and dependencies are outside this gate.

```sh
./bin/check
./bin/snap check apps/testy
./bin/snap test apps/testy             # memory
./bin/snap test apps/testy native
./bin/snap test apps/testy browser
./bin/snap test apps/testy full
```

`bin/check` formats, lints and tests the default packages, compiles portable libraries
for `wasm32v1-none`, checks feature dependency isolation, and runs Testy's opt-in Store
composition. It excludes browser/process tests and Hegel exploration.

The CLI discovers the nearest `snap.toml`. The checkout `bin/snap` wrapper runs from
the repository root, so pass the app directory. `[check].rust` selects fast packages,
defaulting to the application's `Cargo.toml`; `[check].commands` adds source checks.
Each `[test.<platform>]` declares nonempty literal `commands`, run from the app directory
with `SNAP_TEST_PLATFORM` set. The commands own any required build preparation.

`test` defaults to memory. `full` runs check, then declared memory/native/workers/browser/
full suites in that order. Unsupported suites fail rather than skip. `wasm` is rejected
because Workers and browser have different host contracts; Testy has no Workers suite.
Slow memory cases may use `#[ignore = "memory suite"]` and an explicit `--include-ignored`
suite. Browser and stress cases belong in separate targets.

The CLI no longer packages or watches applications. Use `bin/build` and `bin/dev` for
Testy. Former `[server]`, `[web]`, `[prepare]`, `[dev]` and suite `build` configuration
are rejected instead of silently selecting the removed host workflow.

```sh
# Portable behavior and Testy's in-process SDK:
mise exec -- cargo test -p snap-execution -p snap-transport -p snap-store
mise exec -- cargo test -p testy-local --test memory
mise exec -- cargo test -p testy-local --features native --test native

# SQLite, migrations and cross-module signup:
mise exec -- cargo test -p snap-sqlite
mise exec -- cargo test -p snap-sqlite --test recovery -- --ignored
mise exec -- cargo test -p snap-cli
mise exec -- cargo test -p testy-local --features store --test store

# CLI checks, suite selection, cancellation and dependency enforcement:
mise exec -- cargo build -p snap-cli
mise exec -- python3 tests/cli/check.py
mise exec -- python3 tests/cli/architecture.py

# Required after package/dependency changes:
mise exec -- ./bin/snap check apps/testy --structure-only --workspace

# Optional dependency advisory check:
mise exec -- ./bin/check-deps --audit
```

SQLite's abrupt-process recovery test is an explicit pre-handoff gate. Ordinary Store
tests cover resident reads, rollback and publication. SQLite tests cover constraints,
migration rollback, restart and exclusive ownership. See [Store](docs/store.md) for
the durability contract and the generated model.

Memory delivery passes values without encoding and yields before dispatch. A virtual
clock drives connection expiry. Execution contracts cover the whole-operation gate,
admission, repeated dependency requests, rollback, deferred scope release, replacement
and replay. Native tests own socket loss and competing attachments.

The browser gate builds the Wasm SDK and assets, typechecks the UI, then runs Chromium
against an ephemeral WebSocket host:

```sh
bunx playwright install chromium
./bin/check-testy-web
```

Testy's journeys cover routing/bootstrap, health, arithmetic, reload, reconnect/close,
exact 64-bit values, and the execution desk. Development-control tests cover stepping,
dependency supply, snapshots, replacement, pushed reports, command correlation and
disconnects without replay. Latest-report coalescing has an in-process platform test.

## Property testing

`snap-core-properties` registers transport, execution, local-platform and Store
properties. Cases live in their owners' `tests/properties` directories; the separate
`tests/properties/Cargo.toml` owns the pinned host-only Hegel dependency and static
engine. Hegel adds no production dependency or portable feature.

```sh
mise exec -- cargo test --locked -p snap-core-properties
HEGEL_DEFAULT_PROFILE=stress HEGEL_SEED=20260926 \
  mise exec -- cargo test --locked -p snap-core-properties -- --nocapture

# One target, with fresh generation instead of database reuse:
HEGEL_TEST_CASES=10000 HEGEL_SEED=42 HEGEL_DATABASE=disabled \
  mise exec -- cargo test --locked -p snap-core-properties --test store-properties
```

`tests/properties/hegel.toml` defines 200 cases per property for development/CI and
10,000 for stress. Environment variables can override count, seed and persistence.
Local counterexamples live in Git-ignored `tests/properties/.hegel`; Hegel's built-in
CI profile disables that database. Retain failure output when exploring in CI.

On failure, keep the reduced trace and printed `#[hegel::reproduce_failure("...")]`
attribute. Temporarily add it below the failing property's `#[hegel::test]` to replay
with the pinned version. Remove it after fixing the defect, rerun generation, and
preserve important histories as deterministic contract tests. Reproduction blobs are
version-specific; seeds alone do not preserve regressions across code changes.

Generated tests drive sequential host events and check intermediate state. They do
not establish OS scheduling, network timing or crash recovery. Keep those adapter
gates. The dated [Hegel evaluation](docs/hegel-testing-research.md) records the original
transport/execution campaign; [Store verification](docs/store.md#verification) records
the Store campaign and mutation probes.
