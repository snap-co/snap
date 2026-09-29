# Snap Rust spike

Snap is a local experiment in portable transport, serialized application execution,
durable resident Store, Identity, Access, Document and OIDC. Testy and Authy exercise
those contracts with application-owned hosts.

## Start

The active Rust flow requires mise and its pinned Rust toolchain:

```sh
mise trust
mise install rust
mise exec -- rustup target add wasm32v1-none
export PATH="$PWD/bin:$PATH"
./bin/check
```

`bin/check` runs the selected formatting, lint, behavior, portability and dependency
checks. Cargo's default members include the shared capabilities, native platforms,
Testy and Authy. See [TESTING.md](TESTING.md) for
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
or the transport-backed calculator at `/calc`. Create an account or sign in to use
Calc. Each connection owns a fresh calculator. Disconnect, reload, session expiry
and sign-out discard its value and history. Separate sessions and tabs never share
a calculator. Credentials and sessions persist in SQLite; calculator state does not.

For the browser host, install Bun dependencies and the browser Wasm target once:

```sh
bun install
mise exec -- rustup target add wasm32-unknown-unknown
./bin/snap build --project apps/testy
apps/testy/dist/development/server --migrate
./bin/snap dev apps/testy
```

Open `http://127.0.0.1:3848`. The runner builds the Rust SDK binding and web assets,
then starts the host and Vite. Frontend edits hot-reload; Rust edits rebuild the
native host and Wasm SDK, restart the host and reload the browser. Failed builds
keep the previous generation running. Restart/reload discards Calc state while
retaining the tab's login. Configure the listener in the app's deployment TOML.
`cd apps/testy && snap build` creates `dist/development/server`, adjacent assets
and config. Run it from any working directory. `host.data_dir` and `host.database`
select the explicitly migrated Identity database. Startup loads
Identity's tables; it never creates or migrates them. Close the host before migrations.
See [Identity](docs/identity.md) for operation and session contracts.

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

# In separate terminals, run the TCP server and an explicit SDK login:
mise exec -- cargo run -p testy-local --no-default-features --features native --bin testy-server-native -- --config apps/testy/.deployment/development/config.toml
export TESTY_EMAIL='you@example.com' TESTY_PASSWORD='your-test-password'
# Set TESTY_ENROLL=1 only for the first enrollment; omit it for subsequent logins.
mise exec -- cargo run -p testy-local --no-default-features --features native --bin testy-client-native
```

The execution demo starts at 42, attempts +10, and discovers a missing input after
its private write. Live state stays at 42 while a queued +20 waits. Supplying the
input commits 52, then 72. The demo then replaces the addition implementation,
restores the saved 42, and replays +10 under replacement code to produce 62.
Replacement uses ordinary Rust calls; a Wasm/shared-library loader is future work.

The TCP server uses its config's listener; set the demo client's `TESTY_ADDR` to
that address. They
use the same Identity database and plaintext TCP for local experiments. The server's
immediate input resolver supplies a ceiling of 1000 for `calc.add_checked`.
The in-process memory/execution demos explicitly select a fixed test authority;
network hosts accept only Store-backed sessions.

## Network development

Set `host.origin` in `.deployment/development/config.toml` to the app's stable public
origin. Optional `[dev].listen` controls the frontend listener; the backend remains
loopback. For HTTPS behind a proxy, the frontend must also bind loopback. For HTTP
LAN/Tailscale access, explicitly select `0.0.0.0:<port>` for the frontend.

Set the same Authy URL as `[app.oauth].issuer` in Chatty and Factorio. Authy's
`[[app.clients]]` entries declare each client origin. Optional `app.app_domain`
derives exact HTTPS origins per registered client; `app.auto_approve_domain`
controls consent independently. The default development config uses `snapco.dev`.

The proxy admits only discovered/configured authorities and matching Origin headers,
overwrites forwarding headers, and supplies checked alias origins for development
callbacks. Production forbids development-origin lists. Restart runners after config
or network changes. See [configuration and packages](docs/configuration.md).

## Run Authy

```sh
mise exec -- cargo build -p authy-native
target/debug/authy --config apps/authy/.deployment/development/config.toml --migrate
./bin/snap dev apps/authy
```

Open `http://127.0.0.1:3846` to create an account and edit its private profile.
Authy persists credentials, sessions, profile documents and OIDC signing keys in
SQLite. Frontend HMR and native/Wasm rebuilds retain the account and profile.
`cd apps/authy && snap build` creates `dist/development/server` with adjacent web
assets and configuration. Initialize and seal its required secrets first.
See [Authy's contract](apps/authy/CONTRACT.md) for OAuth registration, configuration,
packaged launch and verification commands.

## Run Authy and Chatty together

```sh
# After explicitly initializing and sealing each app's matching client secrets:
mise exec -- bun scripts/chatty.ts --migrate
# Start both dev servers, with frontend HMR and native/Wasm rebuilds:
mise exec -- bun scripts/chatty.ts
```

Open `http://127.0.0.1:3850` and choose Continue with Authy. The runner reads
each app's `.deployment/development/config.toml` and encrypted bag. Chatty synchronizes
conversation messages between clients. See [Chatty](apps/chatty/CONTRACT.md) for
configuration and verification.

## Run Factorio

Factorio coordinates local tickets, exclusive crate claims, Git worktrees and
human-approved integration. See [Factorio](apps/factorio/CONTRACT.md) for the
repository configuration, Authy registration, explicit migration and launch.
`bin/factory help` lists its CLI. `.opencode/commands/factory.md` provides the
repo-local OpenCode V2 command.

In the browser, describe new work to start an OpenCode-backed intake conversation.
The agent explores the repository and saves ticket drafts through Factorio's scoped
tool endpoint. Review the drafts and mark implementation leaves ready. The CLI
equivalent, `bin/factory intake -- <description>`, opens the same conversation in
OpenCode's own terminal UI. Intake requires Bun and an authenticated OpenCode V2
service with a configured model; packaged builds include its client adapter.

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
| [`crates/identity/src`](crates/identity/src) | Credentials, sessions and portable operation dispatch |
| [`platforms/crypto/src`](platforms/crypto/src) | Native password hashing, token digests and secure randomness |
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

Transport and Store underpin the current Identity, Access, Document and OIDC
capabilities. Identity owns credentials and sessions; Passport is not a separate
concept. Document owns optimistic client state and recovery.

Testy, Authy and Chatty have supported native hosts. The obsolete Authy/Chatty
Workers compositions have been removed. Outbound HTTP has a portable contract;
streamed model generation and tools run in native hosts.

The CLI owns `dev`, `build`, `check`, `test`, `migrate` and `secrets`. `snap build
[environment]` owns conventional compilation and deployment packaging; apps do not
declare build commands. Development commands retain owned process groups.
Testy's dev driver owns its
Vite and Rust/Wasm reload workflow. `bin/dev` and `bin/build` remain Testy shortcuts.
