# Snap Rust spike

A local architecture experiment in IO-free Rust providers, driven by native and
browser hosts. Healthy demonstrates a health-check client; Authy adds persistent
password sessions. This is a selected Snap compatibility slice, not a complete port.

## Start

Requires Linux, Bun, Node.js, lsof, and mise. Install Chromium separately for browser tests.

```sh
mise trust
mise install
mise exec -- ./bin/snap dev apps/authy
# Or launch Healthy:
./bin/dev
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
mise exec -- ./bin/check
```

Builds produce `.snap/build/{debug,release}` under the app, containing an executable
and adjacent `web/` assets. The package runs on a compatible host without a Rust or
JS toolchain. TLS and process supervision belong to the deployment environment.

`snap` searches upward for the nearest `snap.toml`; paths resolve relative to that
file. See [Healthy's config](apps/healthy/snap.toml) and
[Authy's config](apps/authy/snap.toml) for working examples. To use the checkout CLI
directly as `snap`, run `mise run cli` with mise activated in your shell.

Standalone hosts use `SNAP_ADDR` for the listen address and `SNAP_WEB_DIR` for an
asset-directory override. Authy's database, origin, and signing-key configuration
are in [its contract](apps/authy/CONTRACT.md).

## Local Cloudflare Workers

`mise install` installs the pinned `worker-build`; `bun install` installs Wrangler.
Wrangler runs the Rust/Wasm application in local `workerd`, including SQLite-backed
Durable Objects. No Cloudflare account is needed for these local commands.

```sh
# Build Authy's browser assets, then run its Workers composition on port 8788:
mise exec -- ./bin/snap build apps/authy
mise exec -- bunx wrangler dev --cwd apps/authy/workers --local --inspector-port 0

# Or run the small stateless Healthy Worker on port 8787:
mise exec -- bunx wrangler dev --cwd apps/healthy/workers --local --inspector-port 0
```

Authy opens at `http://127.0.0.1:8788`. Its Worker data lives under
`apps/authy/workers/.wrangler/state`, independently of the native Authy database.
Rebuild browser assets after UI/client changes. Wrangler builds the server Wasm
with `worker-build --release`. Run commands from the Worker directory or use
`--cwd`; `--config` alone does not set the custom build's working directory.

The same Wrangler configuration supports deployment. Set `SNAP_ORIGIN` to the
public HTTPS origin before deploying Authy, select its custom domain or workers.dev
hostname, and keep `SNAP_REALM` stable. The default origin is local-only. Cloudflare
version metadata supplies the server Build; `SNAP_BUILD` is an explicit test override.
Cloud deployment, CPU-budget measurement on Cloudflare, and automated branch
Previews are not established by the local tests. See the Workers guarantees in
[Authy's contract](apps/authy/CONTRACT.md#workers-host).

## Read when needed

- [ARCHITECTURE.md](ARCHITECTURE.md): ownership, module ports, portability, storage.
- [TESTING.md](TESTING.md): test policy, prerequisites, verification entry points.
- [Authy contract](apps/authy/CONTRACT.md): persistence, wire compatibility, recovery.

Document/Snapshot replication, native language bindings, deployment automation, and
full TypeScript Protocol compatibility are future work.
