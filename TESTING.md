# Testing contracts

The suite should survive replacing the server implementation or client runtime.
Queue changes, crate splits, state representations, and a language rewrite should
require at most a different launcher or client adapter, not rewritten assertions.

## Default to the consumer's interface

The main behavior suite runs outside the implementation:

- **Headless journeys** run realistic multi-step client workflows through the native
  SDK. The script is the client application consumer, with no renderer or binding.
  Completion conditions and a runner deadline turn failed or stalled steps into
  an unsuccessful run; journeys do not inspect runtime internals.
- **Client SDK contracts** exercise operations, subscriptions, state updates, and
  observable lifecycle behavior through the SDK that applications use.
- **Protocol contracts** exercise wire behavior the SDK hides: envelopes, malformed
  input, status and close codes, correlation, and compatibility negotiation.
- **Host CLI contracts** exercise launch, replacement, and shutdown through the real
  executable. Process lifecycle is an observable interface too.

Keep one primary assertion of each promise. A raw-protocol case complements an SDK
case only when it proves something different. A Rust function being public does
not make it an application contract. Tests should not import dispatchers,
controllers, private stores, or call `Module::update` to retest client behavior.

Use a platform-interface contract only for a promise that cannot be expressed at
these consumer interfaces. State the uncovered promise before adding it. Keep its
assertions at the replaceable interface, including injected failures or time when
needed. Do not infer a need for tests from a file, function, branch, or coverage gap.

## What assertions may depend on

Assert returned values, public errors, authorized delivery, documented ordering,
and externally visible lifecycle outcomes. Do not assert helper calls, allocation
counts, queue contents, SQL text, incidental event batching, or private layouts.
Use the same scenario against alternate implementations through their launchers or
client adapters; avoid cloning suites per platform or making every possible matrix
combination a required run.

Keep expected outcomes independent of the implementation under test. Retain a
small set of wire examples even if the server and SDK eventually share Rust code.
That prevents the two sides agreeing on the same mistaken encoding.

Regression tests reproduce the reported behavior at its owning consumer interface.
Use explicit readiness/completion signals; timeouts bound a stalled test rather
than define correctness. Fixtures own their processes and release them in cleanup.
Network tests use OS-assigned ports. Development-port replacement is reserved for
the development runner and its lifecycle contract.

## Current checks

```text
tests/
  journeys/       Native SDK scripts, driven by platform runners
  sdk/            Public SDK contracts and controlled failure scenarios
  protocol/       Wire promises the SDK hides
  browser/        Packaged application, bindings, and rendered observations
  adapters/       Process ownership, client construction, controlled wire peers
  cli/            Local snap command contracts against independent project fixtures
```

`tests/journeys/healthy.rs` waits for a successful observation through the native
client SDK. Its runner in `apps/healthy/native/examples/healthy-journey.rs` owns the
runtime, configuration, deadline, and cleanup. It can run against a development
server independently of the test suite.

`tests/sdk/healthy.contract.ts` contains the reusable health assertion. The reference
TypeScript adapter, native Rust process bridge, and Rust/WASM facade all run it.
The bridge only translates calls/results; it contains no assertions.

`tests/sdk/native.rs`, compiled by the `healthy-native` package, runs Rust SDK contracts
directly against a controlled HTTP peer: error propagation, correlation, concurrency,
cancellation, deadline expiry,
and resident-client failure/recovery. `tests/sdk/browser-runtime.test.ts` checks
the browser SDK's corresponding query behavior and the binding-specific promises
of immutable snapshot identity and subscription cleanup. Neither suite uses React.
Both exercise real IO adapters; no simulated runtime is claimed by these tests.

`tests/protocol/healthy.contract.ts` checks independent wire examples, malformed
input, methods, routes, and Build discovery. `scripts/healthy-smoke.ts` runs this
contract, all three health SDK adapters, and the native journey. Its default
launcher owns a Rust process on an ephemeral port. Set
`SNAP_BASE_URL` to run the same assertions against an already-running compatible
server. Set `SNAP_REFERENCE` to select the TypeScript checkout, defaulting to
`~/code/bod/snap` with dependencies installed.

`tests/browser/healthy.spec.ts` launches `dist/healthy` with no asset-path override.
Chromium loads the real HTML/JS/WASM and verifies OK, failed polling, history, and
recovery. This catches packaging and rendering faults beyond the headless SDK
contracts. Playwright interception supplies a network failure at the browser edge.

`tests/cli/dev.py` runs the real Rust `snap` executable against temporary projects.
It covers discovery from nested/explicit directories, invalid nearest config,
literal hook arguments/order/cwd, failure exit codes, Cargo target selection, and
signal cleanup of hook descendants. Its small Rust fixture is an independent CLI
consumer, not an internal runtime test. Run through mise if Cargo is not on PATH.
The fixture launcher requests shutdown on timeout before forced session cleanup;
stalled-command checks verify both paths release their listeners. A wrapper test
also verifies mise-only Cargo availability, when mise and Bun are installed and
Cargo is absent from the system default PATH.
Startup contracts also cover a host that closes stderr and stalls, a healthy HTTP
host with closed stderr, and an early-exiting host whose descendant retains stderr.
Snap must enforce its readiness deadline independently of all three logging cases.
Port-zero fixtures verify Snap passes a concrete port to the child. The lifecycle
smoke test reuses both prescribed addresses to verify replacement and cleanup.
The watcher CLI contract edits a dependency's separate workspace manifest and
creates an initially absent ancestor Cargo config. HTTP responses expose the
changed compiled values, without a Rust source edit to trigger recovery.

`tests/cli/build.py` uses the same process fixture for the build command. It runs
packaged native binaries and an independent generated WASM/browser application to
verify debug/release profiles, directory selection, example targets, literal hook
ordering, failure recovery, listener preservation, concurrent-build rejection, and
interruption cleanup. A relocated native package runs after its sources and Cargo
outputs are removed. These are CLI/output contracts, not Rust helper tests.

`tests/cli/check.py` verifies selected-project checks, artifact prerequisites, literal
arguments, sibling isolation, failure/missing-tool reporting, and descendant cleanup.
`snap check apps/healthy` runs application Rust, TypeScript, SDK, and packaged browser
checks without the reference checkout. The full repository gate adds CLI and
cross-implementation compatibility assertions.

`tests/cli/architecture.py` verifies actionable errors for forbidden dependencies,
including feature/target-specific and development edges, missing role declarations,
and automatic portability checking of a newly declared workspace package. Assertions
use `snap check --structure-only`; they do not import the metadata implementation.
Project checks include warnings-denied Rustdoc. The repository gate also checks all
workspace documentation and pinned dependency tools. `bin/check-deps --audit` opts
into network advisory checks separately.

`scripts/dev-smoke.py` starts two real `snap dev` processes from Healthy's root on
the same dynamically selected port. It proves replacement, HTML/WASM serving, and
that terminating the CLI releases the listener. Both scripts use Python's standard
library. `tests/browser/dev.spec.ts` verifies the development assets boot in Chromium;
the release browser test separately verifies portable artifact packaging and recovery.

`tests/browser/hmr.spec.ts` edits an owned copy of Healthy's renderer/CSS. It checks
Fast Refresh preserves component state, page identity, Rust sample history, and Build
identity. CSS updates without navigation. Shutdown checks both public frontend and
private backend listeners. Fixture sources and outputs are removed afterward.

`tests/browser/rust-watch.spec.ts` owns a copy of Healthy's Rust application/native/
WASM sources. It proves native restart, WASM-only reload without native restart,
shared dependency rebuilding, failed-compilation retention, edits during a gated
build, startup-failure rollback, configuration recovery, and interruption during a
manifest-triggered rebuild. Assertions use rendered bindings, HTTP Build discovery,
CLI diagnostics and process lifecycle. Generated output never drives source edits.

`tests/browser/dev-outputs.spec.ts` keeps dev live while a separate `snap build`
publishes a different generated binding ABI, selected by a command-local build-script
environment variable. The packaged browser observes its numeric result while fresh
dev pages retain their string result. Subsequent WASM and native edits still reload
correctly. This covers initial and watched dev output ownership through the CLI and
real browser, including JS/WASM pairing rather than identical-file publication.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
snap check apps/healthy --structure-only --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
./bin/check-deps
cargo build -p snap-native --examples
cargo build -p healthy-native --bins --examples
cargo build -p snap-cli
python3 tests/cli/dev.py
python3 tests/cli/build.py
python3 tests/cli/check.py
python3 tests/cli/architecture.py
cargo test -p healthy-native --test client-contract
./bin/build
bun scripts/check-client.ts
bun test tests/sdk/browser-runtime.test.ts
bun scripts/healthy-smoke.ts
bunx playwright test
python3 scripts/dev-smoke.py
```

Install Chromium once with `bunx playwright install chromium`. If the system's
temporary directory is quota-limited, set `TMPDIR` to a writable build directory.
`./bin/check` runs these gates in order. It expects Chromium and the reference
checkout to be installed; ordinary builds and development do not use the reference.

Use `mise exec -- cargo ...` if mise is not activated in the shell. During iteration,
run the check for the changed interface. At a milestone run the relevant gates
once; repeat only after relevant changes or failures. Compiler/lint/portability
checks enforce structure without coupling behavior tests to implementation.
