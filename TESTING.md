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

The active workspace includes the shared capabilities, OIDC issuer/relying party,
native adapters, Testy, Authy, Chatty, the CLI and property consumers.

```sh
./bin/check
./bin/snap check apps/testy
./bin/snap test apps/testy             # memory
./bin/snap test apps/testy native
./bin/snap test apps/testy browser
./bin/snap test apps/testy full
./bin/snap test apps/authy full
./bin/snap test apps/chatty full
./bin/snap test apps/factorio full
```

`bin/check` formats, lints and tests the default packages, compiles portable libraries
for `wasm32v1-none`, checks feature dependency isolation, and runs Testy's opt-in Store
and Identity compositions. It excludes real password hashing, browser/process tests
and Hegel exploration. Memory execution fixtures explicitly select a fixed test authority;
the Identity composition tests use real SQLite with deterministic test crypto.

Authy's app gate runs portable account/issuer tests, real HTTP OIDC integration
with independent signature verification, and browser account/consent journeys.
`bin/check-authy-web` builds bindings/assets and typechecks before Chromium.
`TMPDIR=/tmp/opencode mise exec -- bun test tests/cli/authy-dev.test.ts` exercises
failed-build retention, native/Wasm replacement, profile/session persistence,
CSS/React HMR and process shutdown in a disposable source copy. Authy tests own
fresh explicitly migrated databases and ephemeral listeners. Workers is unsupported.

Chatty's core tests cover message deduplication, atomic rollback, verified senders,
Access and retained deletion. Its browser gate starts real Authy and Chatty hosts,
covering OAuth, cross-client synchronization, denied writes before ACK, restart and
logout. Its native OAuth gate checks pinned RSA verification.
`bun test platforms/document/tests/client.test.ts` checks shared invocation channels.
`bun test platforms/browser/runtime.test.ts` checks anonymous/error startup,
acquisition racing router resolution, initial manifest readiness, identity-epoch
fences, reconnect preservation and disposal during pending IO. All three app
browser gates exercise the shared React kit. Authy covers startup retry and guarded
deep links; Factorio covers empty-account onboarding, intake routes and back/forward;
Chatty covers search-state navigation without another connection.
`bun test tests/cli/chatty-dev.test.ts` covers the shared dev driver.
Set `TMPDIR=/tmp/opencode` for disposable filesystem gates.

`mise exec -- bun test tests/cli/dev-network.test.ts tests/cli/dev-origins.test.ts`
checks discovered-origin policy and real development OAuth across multiple local
addresses, including callback/logout return addresses, authenticated WebSockets,
HMR connections and rejected foreign Host/Origin pairs. Build `authy-native` and
`chatty-native` and Authy's web assets first. The dev-origin journey owns temporary
databases and uses the shared Chatty development runner.

Factorio's linked-Document domain tests cover inherited workspace ownership,
multi-Document rollback, exclusive claims, intake revision guards, retained cleanup,
and atomic ticket/session completion. Its separate property consumer runs with
`mise exec -- cargo test --locked -p factorio-properties`. The resource-ownership
model varies unauthorized callers, conflicting starts, transaction rejection and
cleanup, checking port allocation, claims and finalizers against committed outcomes.

Factorio's full gate covers portable claim/graph/lifecycle rules, native Git effects,
and a real Authy OAuth CLI/browser journey in disposable repositories. The journey
uses an explicit OpenCode V2 contract fixture. Candidate approval is a fixture-only
browser action. Live V2 service compatibility requires an installed V2 CLI.

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

`snap build apps/testy` and `snap dev apps/testy` run the app's `[build].commands`
and `[dev].commands`. These use the same literal command and process-group ownership
as test suites. Testy's dev driver provides frontend HMR and Rust/Wasm rebuilds.
Former `[server]`, `[web]`, `[prepare]` and suite `build` configuration remain rejected.

Run `mise exec -- cargo test -p snap-cli` for CLI dispatch and migration coverage.
After building the CLI, `bun test tests/cli/dev.test.ts` is the explicit development
workflow gate (requires installed Playwright Chromium). It uses a disposable source
copy and migrated database, shares the build cache, and checks CSS/React hot reload,
failed-build retention, Rust/Wasm replacement, login survival and process shutdown.

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
mise exec -- cargo test -p snap-identity
mise exec -- cargo test -p snap-access
mise exec -- cargo test -p snap-document -p snap-document-local
mise exec -- cargo test -p snap-document-local --test lifecycle -- --ignored
mise exec -- cargo test -p testy-local --features identity --test identity
mise exec -- cargo test -p snap-crypto --test native -- --ignored

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

Testy's journeys cover enrollment/login, per-session/per-connection isolation, sign-out,
health, arithmetic, fresh state after reload/reconnect/close,
exact 64-bit values, and the execution desk. Development-control tests cover stepping,
dependency supply, snapshots, replacement, pushed reports, command correlation and
disconnects without replay. Latest-report coalescing has an in-process platform test.

## Property testing

`snap-core-properties` registers transport, execution, local-platform, Store, Identity
and Document-host
properties. Cases live in their owners' `tests/properties` directories; the separate
`tests/properties/Cargo.toml` owns the pinned host-only Hegel dependency and static
engine. Hegel adds no production dependency or portable feature.
`apps/testy/properties` is a separate application-owned composition consumer. It runs
the real Testy SDK, Identity and Store through memory transport with a virtual clock
and deterministic crypto. Its Hegel settings match the core runner; application
dependencies do not enter the reusable core test consumer.
Its held-operation property varies expiry and dependency failure while logout waits
in the application FIFO. Accepted work drains before connection release. The local
platform property separately checks rejection of unaccepted work after retirement.

```sh
mise exec -- cargo test --locked -p snap-core-properties
mise exec -- cargo test --locked -p testy-properties
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

`document-host-properties` generates Access changes, accepted mutations, duplicate
delivery, physical loss, logical close and expiry through the real Store-backed
host. It compares committed counters and controller effects with a separate model.
Store properties also compare net committed-change notifications with before/after
model records. Deterministic host tests cover held controller IO, progress delivery,
explicit blocked-state retry and logical residency reference counts.

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
