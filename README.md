# Snap Rust spike

A local architecture experiment in IO-free Rust providers, driven by native and
browser hosts. Healthy demonstrates a health-check client; Authy adds persistent
password sessions. This is a selected Snap compatibility slice, not a complete port.

## Start

Requires Linux, Bun, lsof, and mise. Install Chromium separately for browser tests.

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

## Read when needed

- [ARCHITECTURE.md](ARCHITECTURE.md): ownership, module ports, portability, storage.
- [TESTING.md](TESTING.md): test policy, prerequisites, verification entry points.
- [Authy contract](apps/authy/CONTRACT.md): persistence, wire compatibility, recovery.

Document/Snapshot replication, native language bindings, deployment providers, and
full TypeScript Protocol compatibility are future work.
