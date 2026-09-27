# Snap Rust spike

Snap is a local experiment in portable transport, serialized application execution,
and durable resident Store. Testy exercises those contracts with application-owned hosts.

## Start

The active Rust flow requires mise and its pinned Rust toolchain:

```sh
mise trust
mise install rust
mise exec -- rustup target add wasm32v1-none
./bin/check
```

`bin/check` runs the selected formatting, lint, behavior, portability and dependency
checks. Cargo's default members select execution, transport, Store, the local and
SQLite platforms, and Testy's library/composition. See [TESTING.md](TESTING.md) for
narrower commands and the explicit Hegel property suite.

App-scoped gates select Testy's memory, native or browser suites:

```sh
./bin/snap check apps/testy
./bin/snap test apps/testy
./bin/snap test apps/testy native
./bin/snap test apps/testy full
```

See [test gates](TESTING.md#current-commands) for configuration.

## Run Testy

Testy is a collection of mini-apps. The launcher at `/` opens Healthy at `/healthy`
or the transport-backed calculator at `/calc`. Entering `/calc` calls `calc.start`;
reload and reconnect retain the tab's calculator until close or expiry.

For the browser host, install Bun dependencies and the browser Wasm target once:

```sh
bun install
mise exec -- rustup target add wasm32-unknown-unknown
./bin/dev
```

Open `http://127.0.0.1:3848`. The runner builds the Rust SDK binding and web assets,
then starts the host. Re-run it after source changes. `TESTY_WEB_ADDR` overrides the
loopback address; `TESTY_WEB_DIR` overrides assets. `bin/build` creates `dist/testy-web`
and adjacent assets for the same local development host.

The execution desk exposes hold/run, an after-acceptance breakpoint, single steps,
dependency supply/failure, snapshots and compiled program selection. Agents use the
same controls over a separate development WebSocket, with HTTP available for
one-off tool calls. The desk receives pushed updates and does not poll.
See [development controls](docs/testy-development.md) for
the wire interface and a complete replay example.

```sh
# Rollback, serialized commits, code replacement and snapshot replay:
mise exec -- cargo run -p testy-local --bin testy-execution-demo

# Client SDK and server together, with asynchronous in-memory delivery:
mise exec -- cargo run -p testy-local --bin testy-memory-demo

# In separate terminals, run the TCP server and SDK client:
mise exec -- cargo run -p testy-local --no-default-features --features native --bin testy-server-native
mise exec -- cargo run -p testy-local --no-default-features --features native --bin testy-client-native
```

The execution demo starts at 42, attempts +10, and discovers a missing input after
its private write. Live state stays at 42 while a queued +20 waits. Supplying the
input commits 52, then 72. The demo then replaces the addition implementation,
restores the saved 42, and replays +10 under replacement code to produce 62.
Replacement uses ordinary Rust calls; a Wasm/shared-library loader is future work.

The native programs use `127.0.0.1:3847` by default; `TESTY_ADDR` overrides it. They
are local fixtures using constant credentials and plaintext TCP. The server's
immediate input resolver supplies a ceiling of 1000 for `calc.add_checked`.

## Resident Store

The SQLite-backed Store supports resident hit/miss reads, cross-module transactions
and explicit schema migrations. A miss returns and discards the operation; the host
loads separately and the caller chooses whether to submit another operation.

```sh
./bin/snap migrate --database .snap/testy-store.sqlite --migrations apps/testy/migrations
mise exec -- cargo run -p testy-local --features store --bin testy-store-demo -- .snap/testy-store.sqlite 1
```

See [Store usage and guarantees](docs/store.md) for migrations, indexes, commit
semantics, miss diagnostics and verification commands.

## Source map

| Location | Responsibility |
| --- | --- |
| [`crates/execution/src/program.rs`](crates/execution/src/program.rs) | Application interface: admission, private attempts, input requests and outcomes |
| [`crates/execution/src/executor.rs`](crates/execution/src/executor.rs) | Host-owned state, global gate, commits, replacement and snapshots |
| [`crates/transport/src`](crates/transport/src) | Verified connection context, lifecycle, envelopes and client correlation |
| [`crates/store/src`](crates/store/src) | Resident transactions, indexes and migration declarations |
| [`platforms/sqlite/src`](platforms/sqlite/src) | Durable commits and explicit database migrations |
| [`platforms/local/src`](platforms/local/src) | Composition of transport/execution with memory or native IO |
| [`apps/testy/src/program.rs`](apps/testy/src/program.rs) | Calculator application implementation |
| [`apps/testy/src/client.rs`](apps/testy/src/client.rs) | SDK and shared calculator journey |
| [`apps/testy/local/src`](apps/testy/local/src) | Application-owned executable entry points |
| [`apps/testy/tests`](apps/testy/tests) | Memory/native SDK scenarios |

The SDK and server remain modules of one portable Testy crate. Transport and
execution are independent crates; neither requires Store. See
[ARCHITECTURE.md](ARCHITECTURE.md) for ownership and execution guarantees.

## Scope

The previous Protocol/runtime/client stack, async Store/Cache, Identity/Passport,
OIDC, and their host adapters have been removed. There is one transport and one
Store implementation to build on. Identity, Access and Document are future work.

Authy, Chatty and the HTTP/LLM sources remain outside the Cargo workspace for later
rewrites. They are not runnable, and their app-local documents describe the old
apps rather than current Snap contracts. Their missing dependencies are intentional.

Use `bin/dev` and `bin/build` for Testy. The CLI owns `check`, `test` and `migrate`;
the previous `snap dev/build` packaging workflow has been removed.
