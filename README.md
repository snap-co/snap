# Snap Rust spike

An experiment in a host-driven, IO-free Snap runtime. The first question is whether
an application library can receive normalized inputs and return actions while a
reusable host owns execution and all external work.

## Run Healthy

Build the Rust CLI once from the checkout root:

```sh
mise trust
mise run cli
```

With mise activated in your shell, this checkout's `target/debug` directory takes
precedence on PATH here and in subdirectories. The existing global `snap` remains
unchanged outside the checkout. Re-enter the directory if your shell has not yet
refreshed its environment. Check the selection with `command -v snap`.

Then run from the application root:

```sh
cd apps/healthy
snap dev
```

Without shell activation, `mise exec -- snap dev` selects the same local binary.
From the checkout root, `snap dev apps/healthy` selects it explicitly. The optional
checkout convenience command rebuilds the CLI before launching Healthy:

```sh
./bin/dev
```

Open **http://127.0.0.1:3846**. The React screen observes a resident Rust client
application through WASM bindings. Rust owns polling, health status, and the last
60 samples. React owns rendering only. Polls are single-flight, with a two-second
wait after each completion and a five-second request deadline.

The CLI currently supports Linux. It needs Cargo and `lsof`; browser builds also
need Bun and the `wasm32-unknown-unknown` Rust target. `mise install` installs the
pinned Rust toolchain. `mise run dev` invokes the checkout convenience command.
With rustup on PATH, `./bin/dev` also works without mise. The CLI installs locked JS
dependencies and its pinned wasm-bindgen tool on first use. Restart `snap dev` to
rebuild changes; there is no file watcher or hot reload yet.

```sh
curl -H 'x-snap-operation-id: example-1' http://127.0.0.1:3846/health/up
```

The response uses the existing Snap completion envelope:

```json
{
  "key": "transport.complete",
  "target": "example-1",
  "payload": { "ok": true, "payload": { "status": "OK" } }
}
```

`SNAP_ADDR` overrides the configured listen address, defaulting to `127.0.0.1:3846`.
`snap dev` creates one fresh Build token per invocation unless `SNAP_BUILD` is set.
The standalone host defaults to `rust-spike`. `GET /__snap/build` exposes the Build
document. Ctrl-C or SIGTERM stops the CLI's active process group, including hooks
and builds, with a six-second grace period. The CLI propagates child exit codes.
It builds before replacing an existing current-user listener, sends SIGTERM, and
escalates to SIGKILL if the original listener still holds the port after three seconds.
Port 3000 is already used by local Grafana.

`cargo run -p healthy-native --bin healthy` also uses port 3846, but only `snap dev`
performs development-port replacement. The ordinary server does not kill processes.

### Project configuration

`snap dev [directory]` searches upward from the selected directory for the nearest
`snap.toml`. It never skips an invalid config to launch a parent application.
All configured paths resolve relative to that file. Healthy's config lives at
`apps/healthy/snap.toml`; the complete fields for this milestone are:

```toml
version = 1
application = "healthy"

[server]
manifest = "native/Cargo.toml"
bin = "healthy" # Select exactly one example or bin target.

[web] # Omit this table for a server-only application.
package-dir = "../.."
application = "web/app.tsx"
host = "../../clients/react/main.tsx"
html = "../../clients/react/index.html"
wasm-manifest = "wasm/Cargo.toml"
bindings = ".snap/bindings"

[dev]
address = "127.0.0.1:3846"

[prepare]
dev = [] # Optional arrays of executable + literal arguments, in order.
```

Cargo manifests select packages; Cargo artifact messages identify their build
outputs even with a custom target directory. The global/local `snap` executable
does not link or dynamically import the application's Rust code. It builds the
configured host and launches that executable from the project root.

The optional browser build installs dependencies in `web.package-dir`, builds the
configured WASM crate, generates JS bindings, and bundles the configured application
with the reusable host. The Bun build driver is embedded in the Rust CLI; the app
does not need to reference a checkout script. This spike pins wasm-bindgen 0.2.128.
The selected browser host uses `main.js`, `main.css`, and
`snap_client_wasm_bg.wasm`; the supplied HTML and binding facade must match it.
Assets and CLI build tools live under the application's ignored `.snap/dev/`.
Bindings go to the explicit location imported by its TypeScript facade.

Preparation hooks run once before the build, with the config directory as cwd and
inherited environment/output. Each command is an argument array, not a shell
string. The first failure stops startup. Config loading itself executes no hooks.
`SNAP_ENV` defaults to `development` for the launched host; the CLI supplies
`SNAP_ADDR`, `SNAP_APPLICATION`, `SNAP_BUILD`, and its own `SNAP_WEB_DIR`.

Only `dev`, help, and version are implemented. CLI-client composition, deploy,
infrastructure operations, project creation, and project-pinned CLI dispatch remain
later slices. For installation outside this checkout, `cargo install --path tools/cli`
builds a standalone `snap`; the development toolchain is still needed to build apps.

### Headless journey

With the server running, execute the same client application without a renderer:

```sh
mise exec -- cargo run -p healthy-native --example healthy-journey
```

`SNAP_BASE_URL` selects another server. The journey waits for an OK observation,
then closes the client. A failed health result or a ten-second journey deadline
exits unsuccessfully. The scenario lives in `tests/journeys/healthy.rs`; platform
construction and cleanup live in `apps/healthy/native/examples/healthy-journey.rs`.

### Release artifact

```sh
./bin/build
./dist/healthy
```

The `dist/` directory contains the native executable and its adjacent `web/`
assets. Copy that directory to a compatible host to run it without the checkout,
Node, Bun, or a Rust toolchain. `SNAP_ADDR=0.0.0.0:3846` binds beyond loopback.
`SNAP_WEB_DIR` overrides the asset directory. TLS and process supervision belong
to the deployment environment; no deployment provider is configured here.

## Crates follow dependency constraints

```text
crates/
  protocol/       snap-protocol   Normalized invocation and wire vocabulary
  runtime/        snap-runtime    Module entry point, Transport dispatch, Doctor
  client/         snap-client     IO-free client behavior
bindings/
  wasm/           snap-client-wasm  Shared query marshalling, linked by app WASM crates
clients/
  typescript/                    Shared Promise/observation and initialization support
  react/                         Shared browser entrypoint and rendering host
platforms/
  browser/        snap-browser    Browser Fetch, deadlines, timers, task lifetime
  native/         snap-native     Server host and native client runtime
apps/
  healthy/        healthy         IO-free server and client application definitions
    native/       healthy-native  Native server, journey runner, SDK contract target
    wasm/         healthy-wasm    WASM composition and Healthy exports
    client.ts                     App facade selecting generated WASM bindings
    web/                         React application definition and renderer
    .snap/bindings/               Ignored application-owned generated bindings
tools/
  cli/            snap-cli       Local snap executable, config, builds, process ownership
```

The library dependency graph is:

```text
healthy ──────────► snap-runtime ───────► snap-protocol
    └─────────────────────────────────► snap-protocol

snap-native ──────► snap-runtime
    └────────────► snap-protocol

native Healthy executable composes healthy + snap-native

React → TypeScript facade → WASM binding → Healthy client application
                                              │
Native journey → native client runtime ────────┘

Both runtimes drive the same synchronous client application.
Native runtime → reqwest / Tokio
Browser runtime → browser Fetch / timers through Rust bindings
```

Protocol, runtime, client, and application crates are `no_std` with `alloc`. The native
host supplies allocation and process lifetime. Workspace source forbids unsafe
Rust. Application dependencies contain no Tokio, Axum, filesystem, or SQL client.
The bare-WASM compilation gate catches accidental standard-library dependencies
in the application graph. This is architectural discipline, not plugin sandboxing.

The native executable and WASM exports have separate app-owned packages. Their
platform dependencies cannot silently turn the portable `healthy` package into a
platform binding. Shared native/browser runtimes and binding support do not depend
on Healthy, including through development dependencies. The native SDK contract
target lives in `healthy-native`; its assertions remain in `tests/sdk/native.rs`.

### Why these crates?

Rust modules organize behavior. Crates establish independently compiled dependency
and portability constraints. More crates do not automatically make compilation
faster: changes to a shared interface still rebuild dependants, and generic code
can be instantiated downstream.

The broad TypeScript `core` package does not become a miscellaneous Rust crate.
Protocol is the first shared vocabulary we actually need. Runtime contains the
portable implementation and the small host/module interface. Doctor is currently
one module, not a crate pair named Health and Doctor.

The client core is independently consumed by native clients and language bindings.
The browser runtime has WASM/browser dependencies, and the WASM binding exports
language-facing handles. Neither belongs in the portable client. Swift/Kotlin
bindings can later consume the native SDK without introducing one runtime per language.

Use the same rule when Document arrives. Begin with named document and snapshot
modules. Extract a `snap-document` contract crate when an independent consumer
needs it without the controller. Extract `snap-snapshot` when its implementation
has a useful independent dependency or compilation lifetime. Platform-specific
storage code remains outside that portable implementation.

## One execution turn

```text
HTTP adapter strips method/path/headers/query framing
  → host queues a normalized Invocation
  → host calls Module::update(input, actions)
  → Transport dispatches health.up to Doctor
  → Doctor computes its result in memory
  → Transport appends a Complete action
  → host projects it into an HTTP completion Event
```

One host-owned task owns the application instance. Request tasks enqueue inputs;
they never invoke application code directly. The host reuses and drains the action
buffer. A host Delivery id addresses a response slot independently of the caller's
operation id, so equal operation ids cannot steal each other's HTTP responses.

`Module::operations` supplies startup route metadata. `Module::update` is the
execution entry point. `Transport<State>` holds opaque-to-the-host application
state. The interface is an ordinary statically linked Rust interface, not a stable
DLL ABI or a contiguous-memory checkpoint format.

This first slice performs synchronous dispatch. It has no async handler interface.
It also does not establish a universal one-input/one-output rule: that behavior
belongs to this Query slice. Document controllers will need inputs for IO
completions, readiness, and connection lifetimes, and actions for reconciliation.
Healthy does not yet validate those interfaces, so they are not invented here.

## Direction under investigation

Applications declare desired state, such as the documents a client should observe.
Shared Snap controllers compare that with resident state. Platform adapters perform
loads and delivery; their completions become new observations. The reusable
controller owns shared loads, residency, and authorized interest. The application
does not perform a SQL query or balance retain/release calls itself.

The next meaningful experiment is a session-backed document flow with delayed
loads, shared interests, and interest removal during a load. Preserve the SDK and
Protocol contracts while iterating on both client and server implementations.
Native language bindings, plugin loading, and raw memory snapshots remain separate
design decisions.

## Client SDK spike

Build the Rust client into a browser-loadable WASM module and generated JS/types:

```sh
./bin/build-client
```

The script installs the matching wasm-bindgen CLI into `.tools` on first use.
Generated bindings live in `apps/healthy/.snap/bindings` and are ignored by Git.
The app-owned `healthy-wasm` crate links shared query exports and adds its resident
Healthy application. Its TypeScript facade selects that generated module, so another
application can own different exports and outputs without changing shared packages.

The TypeScript facade is what React or another JS application calls:

```ts
import { createClient } from "./apps/healthy/client";

const client = await createClient({ baseUrl: location.origin, build: "rust-spike" });
const report = await client.health.up();
console.log(report.status);
await client.close();
```

Rust constructs invocations, correlates completions, interprets Protocol errors,
and validates the health result. The Rust browser runtime owns HTTP, deadlines,
and cancellation. TypeScript converts results to Promises and adapts observations
to `subscribe` / `getSnapshot`. Snapshots are immutable JS values with stable
identity between notifications, suitable for React's `useSyncExternalStore`.

`startHealthy` in `apps/healthy/client.ts` boots the resident client application,
returning an initial loading snapshot before networking completes. It owns status
and sample history in Rust.
Closing it cancels IO and timers and releases subscriptions. The native runtime
offers the same application's snapshots through a Rust watch handle.

`clients/typescript/src` owns binding initialization, query shutdown, and observation
subscriptions. It imports no generated WASM module. Each application facade owns an
initialization cache and decodes its own immutable snapshot shape.

`apps/healthy/web/app.tsx` selects `startHealthy` and `HealthMonitor`. The build
resolves that definition into `clients/react/main.tsx`; there is no application-owned
init file. The reusable host loads Build metadata and WASM, starts the client,
mounts React, and unmounts before closing the SDK on page exit.

The client application seam currently executes one external step at a time:
Query, Wait, or Stop. This is enough for Healthy's polling policy, not a claim that
future document or command clients are single-flight. A simulation host can drive
the same synchronous inputs and steps, but no simulation runtime is implemented.
Swift/Kotlin bindings are also future work.

## Compatibility target

Reference checkout: `~/code/bod/snap`, revision
`9689a8ed3108f58233721c2000d2b9ea96259fe7`.

The implemented slice covers Healthy's anonymous `health.up` Query, its completion
envelope, query input rejection, route/method rejection, and Build discovery.
Doctor retains the existing readiness exception to exact Build matching.

Healthy includes its browser monitor and asset hosting. There is no Hooky callback.
Passport/cookie semantics, WebSockets, and full Protocol compatibility remain future
slices. The reference TypeScript SDK, native Rust SDK, and Rust/WASM SDK all
exercise Doctor over the same HTTP host.

## Verification and the testing line

See [TESTING.md](TESTING.md) for contract ownership and commands. The behavior suite
uses consumer interfaces so it can survive a complete implementation rewrite.
The current checks include native journeys, SDK edge contracts, a shared health
contract through three client adapters, Protocol checks, host CLI checks, and a
real-browser check against the release artifact.
