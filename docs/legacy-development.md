# Legacy application development

This reference covers Authy, Chatty and the retired Healthy test fixture, which use the earlier
[integration architecture](legacy-architecture.md). Their source remains in the
workspace for later migration. Run the commands below from the repository root.
The active Testy flow is documented in the [main README](../README.md).

A local architecture experiment in IO-free Rust providers, driven by native and
browser hosts. Healthy demonstrates a health-check client; Authy adds persistent
password sessions, account profiles and an OIDC issuer. This is a selected Snap
compatibility slice, not a complete port.

## Start

Requires Linux, Bun, Node.js, lsof, and mise. Install Chromium separately for browser tests.

```sh
mise trust
mise install
mise exec -- ./bin/snap dev apps/authy
# Or launch the legacy Healthy CLI fixture:
mise exec -- ./bin/snap dev tests/fixtures/healthy
# Authy + Chatty, with keys in .snap/chatty.env:
mise exec -- bun scripts/chatty.ts
```

Open http://127.0.0.1:3846. Authy stores accounts and sessions in
`apps/authy/.snap/authy.sqlite`. Dev replaces current-user listeners on its selected
ports after building successfully. Stop with Ctrl-C.

React/CSS changes use Vite HMR. Rust changes rebuild automatically; native changes
restart the server, and WASM changes reload the browser. Failed compilation retains
the working version. Stop dev before changing persistent schema registration and
exercise migrations against fixture databases first.

## Build and check

```sh
mise exec -- ./bin/snap build apps/authy --release
mise exec -- ./bin/snap check apps/authy
mise exec -- bash ./bin/check-legacy
```

Builds produce `.snap/build/{debug,release}` under the app, containing an executable
and adjacent `web/` assets. The package runs on a compatible host without a Rust or
JS toolchain. TLS and process supervision belong to the deployment environment.

`snap` searches upward for the nearest `snap.toml`; paths resolve relative to that
file. See [the Healthy fixture config](../tests/fixtures/healthy/snap.toml) and
[Authy's config](../apps/authy/snap.toml) for working examples. To use the checkout CLI
directly as `snap`, run `mise run cli` with mise activated in your shell.

Standalone hosts use `SNAP_ADDR` for the listen address and `SNAP_WEB_DIR` for an
asset-directory override. Authy's database, origin, and signing-key configuration
are in [its contract](../apps/authy/CONTRACT.md).

Authy's OIDC discovery is at `/.well-known/openid-configuration`. Configure
`CHATTY_ORIGIN` for the registered relying-party origin and `CHATTY_CLIENT_SECRET`
for confidential-client authentication. Both apps must use the same secret. The
registered redirects are `/auth/callback` and `/auth/logged-out` on Chatty's origin.
The pair runner opens Chatty at `http://127.0.0.1:3850`. See
[Chatty's contract](../apps/chatty/CONTRACT.md) for persistence, model/tools, limits,
Workers composition and the Achilles launch command.

## Local Cloudflare Workers

`mise install` installs the pinned `worker-build`; `bun install` installs Wrangler.
Wrangler runs the Rust/Wasm application in local `workerd`, including SQLite-backed
Durable Objects. No Cloudflare account is needed for these local commands.

```sh
# Build Authy's browser assets, then run its Workers composition on port 8788:
mise exec -- ./bin/snap build apps/authy
mise exec -- bunx wrangler dev --cwd apps/authy/workers --local --inspector-port 0

# Or run the small stateless Healthy Worker on port 8787:
mise exec -- bunx wrangler dev --cwd tests/fixtures/healthy/workers --local --inspector-port 0
```

Authy opens at `http://127.0.0.1:8788`. Its Worker data lives under
`apps/authy/workers/.wrangler/state`, independently of the native Authy database.
To use another device on LAN or Tailscale, bind the listener to `0.0.0.0` and
override `SNAP_ORIGIN` with the exact browser origin. For this machine's tailnet name:

```sh
mise exec -- bunx wrangler dev --cwd apps/authy/workers --local --ip 0.0.0.0 --var SNAP_ORIGIN:http://achilles:8788 --inspector-port 0
```

Open `http://achilles:8788` from a device connected to the tailnet. The origin
override is also used for WebSocket admission, so use that hostname consistently.

Stop Wrangler before rebuilding browser assets after UI/client changes, then
restart it so its ASSETS binding uses the new directory. Wrangler builds server Wasm
with `worker-build --release`. Run commands from the Worker directory or use
`--cwd`; `--config` alone does not set the custom build's working directory.

The same Wrangler configuration supports deployment. Set `SNAP_ORIGIN` to the
public HTTPS origin before deploying Authy, select its custom domain or workers.dev
hostname, and keep `SNAP_REALM` stable. The default origin is local-only. Cloudflare
version metadata supplies the server Build; `SNAP_BUILD` is an explicit test override.
Cloud deployment, CPU-budget measurement on Cloudflare, and automated branch
Previews are not established by the local tests. See the Workers guarantees in
[Authy's contract](../apps/authy/CONTRACT.md#workers-host).

## Legacy verification

`bin/check-legacy` preserves the cross-application gate. It includes build, browser,
development-server and Workers checks and is outside the active Testy iteration
loop. `snap check` still runs each legacy app's configured commands. Testy's
`snap.toml` selects structural checks; its browser runner is `bin/dev`.
There is no `snap test` command yet.

The earlier memory rig uses Store, Identity and Passport fixtures. Its snapshots
copy database state, not live futures, clocks, client-held tokens or crypto counters:

```sh
mise exec -- cargo test -p snap-memory -p authy
mise exec -- ./bin/snap check apps/authy
mise exec -- bash ./bin/check-legacy
```

`snap_memory::Rig` owns its local executor, value delivery, virtual time and trace.
Use `run_until_stalled` to inspect held work; `run` and `complete` require work that
can finish without outside actions. Advance its clock explicitly for deadlines.
Reset tests with fresh clients, execution state and compatible crypto state;
restoring only the database is not a complete platform reset.

Install Chromium with `bunx playwright install chromium`. The legacy full gate also
requires `~/code/bod/snap` for TypeScript reference compatibility checks. Ordinary
builds and app checks do not require that checkout. Set `SNAP_REFERENCE` to use a
different reference location for the Healthy smoke program.

Workers tests use local workerd with isolated temporary persistence and ephemeral
ports, without a Cloudflare account. Install the pinned worker-build using mise,
then JS tooling with `bun install`. Examples:

```sh
mise exec -- bun test tests/protocol/workers.test.ts
mise exec -- bun test tests/protocol/carriers.test.ts
```

Legacy suites live under `tests/{sdk,protocol,browser,cli,store,journeys}` and still
need migration to their owning apps/packages. Consult `bin/check-legacy` and each
app's `snap.toml` for their command sequences. Shared test policies remain in
[TESTING.md](../TESTING.md). Authy's authoritative persistence, wire and recovery
contract is [apps/authy/CONTRACT.md](../apps/authy/CONTRACT.md).
