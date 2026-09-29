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
./bin/snap migrate --database .snap/testy-identity.sqlite --migrations crates/identity/migrations
./bin/snap dev apps/testy
```

Open `http://127.0.0.1:3848`. The runner builds the Rust SDK binding and web assets,
then starts the host and Vite. Frontend edits hot-reload; Rust edits rebuild the
native host and Wasm SDK, restart the host and reload the browser. Failed builds
keep the previous generation running. Restart/reload discards Calc state while
retaining the tab's login. `TESTY_WEB_ADDR` overrides the public loopback address.
`./bin/snap build apps/testy` creates `dist/testy-web` and adjacent assets for the
same local development host. Run `./dist/testy-web` from the repository root.
`TESTY_DATABASE` selects the explicitly migrated Identity database. Startup loads
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
mise exec -- cargo run -p testy-local --no-default-features --features native --bin testy-server-native
export TESTY_EMAIL='you@example.com' TESTY_PASSWORD='your-test-password'
# Set TESTY_ENROLL=1 only for the first enrollment; omit it for subsequent logins.
mise exec -- cargo run -p testy-local --no-default-features --features native --bin testy-client-native
```

The execution demo starts at 42, attempts +10, and discovers a missing input after
its private write. Live state stays at 42 while a queued +20 waits. Supplying the
input commits 52, then 72. The demo then replaces the addition implementation,
restores the saved 42, and replays +10 under replacement code to produce 62.
Replacement uses ordinary Rust calls; a Wasm/shared-library loader is future work.

The native programs use `127.0.0.1:3847` by default; `TESTY_ADDR` overrides it. They
use the same Identity database and plaintext TCP for local experiments. The server's
immediate input resolver supplies a ceiling of 1000 for `calc.add_checked`.
The in-process memory/execution demos explicitly select a fixed test authority;
network hosts accept only Store-backed sessions.

## Network development

For HTTPS behind a local reverse proxy, set `SNAP_ORIGIN` to the public HTTPS
origin and bind the dev frontend to loopback with `<APP>_WEB_ADDR`. For example:

```sh
SNAP_ORIGIN=https://authy.cc.example.test AUTHY_WEB_ADDR=127.0.0.1:3846 AUTHY_APP_DOMAIN=cc.example.test ./bin/snap dev apps/authy
SNAP_ORIGIN=https://factorio.cc.example.test FACTORIO_WEB_ADDR=127.0.0.1:3852 AUTHY_ORIGIN=https://authy.cc.example.test ./bin/snap dev apps/factorio
```

The proxy must preserve Host and Origin. The dev server admits only the configured
HTTPS authority in this mode, ignores incoming forwarding headers, and refuses
non-loopback bind addresses. HMR uses the browser's HTTPS hostname over WSS.
Direct LAN HTTP aliases are available only in the HTTP development mode below.
`AUTHY_APP_DOMAIN` is Authy's optional configuration for deriving exact HTTPS
callback and logout URLs for each registered client ID. It overrides individual
client origin variables. It does not register unknown clients or allow one client
to redirect to another client's subdomain. Leave it unset to use explicit origins.

Authy skips OAuth consent for registered HTTPS callbacks on `snapco.dev` and its
subdomains. Set `AUTHY_AUTO_APPROVE_DOMAIN` to replace this domain, or to an empty
value to require consent everywhere. This is independent of `AUTHY_APP_DOMAIN`:
callback registration remains exact, and login is still required when no live
Authy session exists. `prompt=consent` explicitly requests the permission screen.

Authy, Chatty and Factorio's `snap dev` runners listen on `0.0.0.0` by default.
They discover local IPv4 addresses, including LAN and Tailscale, plus the local
Tailscale DNS name and short name when those resolve to this machine. Each address
gets the same frontend HMR and Rust/Wasm rebuild workflow. The native backend stays
on loopback behind the development proxy.

`AUTHY_WEB_ADDR`, `CHATTY_WEB_ADDR` and `FACTORIO_WEB_ADDR` override the listen address
and port. `SNAP_ORIGIN` sets the current app's canonical public origin independently
of its bind address. Without it, the runner uses `http://127.0.0.1:<port>`.

Keep Authy's `SNAP_ORIGIN` stable and reachable from your development devices. Set
the same URL as `AUTHY_ORIGIN` in Chatty and Factorio. Run Authy in dev mode too so it
registers exact callbacks for discovered addresses. `CHATTY_ORIGIN` and
`FACTORIO_ORIGIN` on Authy select the corresponding app's canonical URL and port;
the defaults are 3850 and 3852. Restart the dev runners after network-address or
port changes. Browser sessions are separate per hostname/IP.

For example, in separate terminals with the existing databases and client secrets:

```sh
SNAP_ORIGIN=http://192.168.0.2:3846 ./bin/snap dev apps/authy
AUTHY_ORIGIN=http://192.168.0.2:3846 ./bin/snap dev apps/factorio
```

The proxy accepts only discovered/configured authorities and requires a browser's
Origin to match the requested authority. It translates those checked requests to
the loopback backend's canonical authority and supplies their external origin for
OAuth callbacks. It overwrites forwarding headers. HMR and application WebSockets
use the address opened in the browser and undergo the same authority checks.
Dev callback registration is enabled only by the runner's `SNAP_DEV_MODE=1` and
explicit origin lists. Packaged hosts ignore dev-origin headers by default.
Production retains explicit exact callback registration and a fixed issuer.

## Run Authy

```sh
mise exec -- cargo build -p authy-native
SNAP_DATABASE=apps/authy/.snap/authy-store.sqlite target/debug/authy --migrate
./bin/snap dev apps/authy
```

Open `http://127.0.0.1:3846` to create an account and edit its private profile.
Authy persists credentials, sessions, profile documents and OIDC signing keys in
SQLite. Frontend HMR and native/Wasm rebuilds retain the account and profile.
`./bin/snap build apps/authy` creates `dist/authy/authy` with adjacent web assets.
See [Authy's contract](apps/authy/CONTRACT.md) for OAuth registration, configuration,
packaged launch and verification commands.

## Run Authy and Chatty together

```sh
# Explicitly build and migrate fresh stores, generating a local client secret if needed:
mise exec -- bun scripts/chatty.ts --migrate
# Start both dev servers, with frontend HMR and native/Wasm rebuilds:
mise exec -- bun scripts/chatty.ts
```

Open `http://127.0.0.1:3850` and choose Continue with Authy. The runner reads
`.snap/chatty.env`; environment variables override it. Chatty stores and synchronizes
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

The CLI owns `dev`, `build`, `check`, `test` and `migrate`. Applications declare
literal `[dev].commands` and `[build].commands` in `snap.toml`; the CLI runs them
from the app directory and owns their process groups. Testy's dev driver owns its
Vite and Rust/Wasm reload workflow. `bin/dev` and `bin/build` remain Testy shortcuts.
