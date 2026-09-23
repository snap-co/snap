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
```

`tests/journeys/healthy.rs` waits for a successful observation through the native
client SDK. Its runner in `platforms/native/examples/healthy-journey.rs` owns the
runtime, configuration, deadline, and cleanup. It can run against a development
server independently of the test suite.

`tests/sdk/healthy.contract.ts` contains the reusable health assertion. The reference
TypeScript adapter, native Rust process bridge, and Rust/WASM facade all run it.
The bridge only translates calls/results; it contains no assertions.

`tests/sdk/native.rs` runs Rust SDK contracts directly against a controlled HTTP
peer: error propagation, correlation, concurrency, cancellation, deadline expiry,
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

`scripts/dev-smoke.py` starts two real development runners on the same dynamically
selected port. It proves replacement and that terminating the runner releases the
listener. It uses only Python's standard library.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p healthy -p snap-client --target wasm32v1-none
cargo build -p snap-native --examples
cargo test -p snap-native --test client-contract
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
