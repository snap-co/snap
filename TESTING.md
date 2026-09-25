# Testing

## Fast iteration

The active `bin/check` should maximize useful coverage per second. The target
is a few seconds on a warm build, roughly two to five, rather than a fixed deadline
for cold compilation. Measure test execution and fixture setup separately from
builds. Investigate individual tests taking more than 100 ms, including setup and
cleanup; this is a design signal, not a timing assertion to put in a test.

The default check should run source checks and fast behavior tests. Real browser,
development-server, cross-process, and cross-host suites run separately as explicit
integration or end-to-end gates before integration, release, or deployment. A slow
test does not belong in the iteration loop merely because it covers important
behavior.

## Ownership and interfaces

Assign each promise to the package that owns its behavior. Snap's reusable Store,
Passport, OIDC, transport, and client-controller guarantees belong to Snap tests.
Authy and Chatty tests own their application policy, composition, and user-facing
behavior. Using an app as an integration fixture does not make the underlying
platform guarantee an app responsibility.

App-owned tests and fixtures live under `apps/<app>/tests`, so they can move with
the app into an independent consumer repository. Shared Rust contracts live with
their owning packages; root-level tests are reserved for Snap-wide integration
and tooling. Snap's test support must not depend on Authy, Chatty, or Healthy.
Cross-app journeys belong to the consuming app when that app owns the journey.
The current root-level app suites still need to move to this layout.

Use Cargo's native test runner for Rust core behavior, without launching the Snap
CLI, an application server, or a browser. Portable production code stays `no_std`
with `alloc`; test code may use host executors and test dependencies.

Choose the cheapest public interface that owns the promise:

- In-process provider and controller tests exercise real behavior with controlled
  clocks, randomness, crypto, and storage where those dependencies are not the
  subject. Calling `Provider::invoke` is appropriate for a provider's contract.
- SDK tests cover the SDK's commands, observations, errors, and lifetime. Shared
  controller cases need not be repeated through every native/browser adapter.
- Protocol tests check encoding, headers, and admission, with independent examples
  so a shared encoding bug cannot make both sides agree on the wrong behavior.
  Use in-process request/response interfaces where possible; use real sockets for
  promises that depend on socket behavior.
- Adapter integration tests verify actual crypto, storage, carrier, and host
  compatibility. Keep the host matrix here rather than in every behavior test.
- CLI and browser tests cover real commands, packaging, rendering, process
  lifetime, and development reload behavior.

Keep one primary home for each promise. A small composition test verifies that the
app wires its dependencies together; it does not repeat their full contract suites.
Tests assert observable results, not private state, SQL text, helper calls, or
incidental event ordering. Assertions should survive internal implementation
changes while the public contract stays the same.

## User journeys

End-to-end journeys follow frequent user processes in their natural order. Chain
the actions a user would take, let failed operations, panics, exceptions, and
timeouts fail the journey, and verify the final observable result. Avoid repeating
fine-grained contract assertions at every step. Actions must propagate failures;
an expected failure that returns a value rather than throwing needs an explicit
check. Readiness waits synchronize the journey, rather than re-proving each
component's behavior.

Keep detailed edge cases and failure rules in focused tests at their owning
interface. Journeys verify that the assembled system delivers a complete result.

## Fixtures and reset points

Test both sides of the application/platform seam. A test platform runs the real
portable application with controlled storage, time, randomness, external results,
and scheduling. It can drive public operations or in-memory HTTP requests without
networking or disk IO. Fault scenarios must respect the dependency contracts,
including their defined failure modes; simulated runs should have reproducible
seeds and schedules.

Conversely, a platform-neutral contract application exercises host obligations
against each real platform adapter. Keep its scenarios shared across hosts and
its construction host-specific. It verifies the contracts that applications rely
on, rather than using a product app as the universal host fixture. Host-specific
behavior still needs host-specific tests. Neither direction requires dynamic
library loading; an in-process Rust composition can exercise the same seam.

Prefer in-process construction and reset for fast behavior tests. When a live
server is needed, a fixture may reuse it if it can restore a known state quickly
and completely. Transaction rollback is sufficient only when all relevant writes
participate; also reset caches, sessions, clocks, pending work, and observations
that could leak between cases. Keep reset controls in test fixtures.

Expensive preparation may produce a reusable non-empty baseline. Build and verify
that baseline once, save it, and give each dependent test an isolated copy or a
complete reset to it. For example, later phases can start with enrolled accounts
and provisioned test signing keys. Tests must not depend on another test running
first or mutate a shared baseline. Rebuild saved fixtures when their schema,
configuration, or preparation inputs change. Use test-only identities and keys.
Tests of fresh provisioning or migrations must still exercise those paths.

Fixtures own temporary data and processes, use ephemeral ports, and clean up on
failure. Synchronize on readiness or completion rather than fixed sleeps; timeouts
bound stalled tests. Tests of elapsed-time behavior should control time where the
interface permits it. Development-port replacement belongs only in the runner's
lifecycle tests.

## Current commands

The active build is Testy, `snap-transport` and `snap-execution`. Run `./bin/check`
for selected formatting, Clippy, transport contracts, memory/native SDK and development-control tests,
portable WASM compilation, and dependency isolation. It does not run or build
Authy/Chatty integration suites. Cargo default members select the five portable/local
packages; the check also compiles the browser binding.
`cargo test -p testy-local` runs the memory SDK scenarios; add `--features native`
for the real TCP host case. Testy scenarios live in `apps/testy/tests`, registered
by its application-owned local composition. Transport contracts live in
`crates/transport/tests`. Execution contracts live in `crates/execution/tests`.

Memory delivery yields before dispatch and passes values without byte encoding.
Its virtual clock drives detached-connection expiry; native tests exercise actual
socket loss and competing attachments without waiting through expiry windows.
Execution tests verify the whole-operation gate, admission ordering, repeated
read misses after tentative edits, rollback of bad commits, FIFO writes, deferred
scope release, stale dependency responses, code replacement and snapshot replay.
Memory SDK tests hold a read while another client's operation queues, then supply
it without wall-clock waits. Cancellation after submission leaves work owned by
the host; connection expiry releases its state after that work finishes. The native
case runs the same SDK journey plus a checked calculation using immediate host
inputs. No test uses Identity, Store, or fixture crypto.

Run `mise exec -- cargo run -p testy-local --bin testy-execution-demo` for a short
asserting demonstration of rollback, serialized writes, retained-state code
replacement, and replay from an in-memory snapshot. It loads no dynamic module.

Focused commands:

```sh
# Portable capability contracts:
mise exec -- cargo test -p snap-execution -p snap-transport

# Testy SDK scenarios:
mise exec -- cargo test -p testy-local --test memory
mise exec -- cargo test -p testy-local --features native --test native

# Required after package/dependency changes:
mise exec -- ./bin/snap check apps/testy --structure-only --workspace

# Optional network-backed dependency advisory check:
mise exec -- ./bin/check-deps --audit
```

The structure-only workspace check is required after package/dependency changes.
It compiles portable legacy packages too, but runs no legacy application suites.
Browser/Workers prerequisites, the earlier memory rig, `snap check` and
`bin/check-legacy` belong to [legacy development](docs/legacy-development.md).

The separate browser gate builds the Wasm SDK and assets, typechecks the Testy UI,
and runs real Chromium against an ephemeral WebSocket host:

```sh
bunx playwright install chromium
./bin/check-testy-web
```

Its app-owned journeys cover routing/bootstrap, health, arithmetic, reload,
reconnect/close, exact 64-bit values, and HTTP tools stepping a live browser request
through a held dependency, restoring state and selecting replacement code. The
development WebSocket cases cover initial/pushed reports, command correlation and
errors, HTTP-to-subscriber updates, independent reconnect, idle silence and lost
command responses without replay. The numeric journey supplies exact i64 inputs
over the debugger socket. A local platform test covers latest-report coalescing
for a slow observer. The in-process development-control journey covers stale input rejection and preservation
of submitted work after its observer disconnects. Browser/process tests stay out
of the warm `bin/check` loop. Healthy's legacy fixture lives under
`tests/fixtures/healthy`; root build/dev commands now select Testy.

Formatting, Clippy, Rustdoc, dependency policy, and structural checks enforce
source constraints separately from behavior tests.
