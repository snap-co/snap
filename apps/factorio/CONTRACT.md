# Factorio

Factorio turns an OpenCode intake conversation into draft tickets, then coordinates
ticket-backed work and human-approved local integration. Authy owns sign-in.

## Setup and onboarding

`[app.repository]` in `.deployment/<environment>/config.toml` describes the existing
server-hosted repository:

```toml
[app.repository]
repository = "/absolute/path/to/repository"
mainline = "main"
modules = { "my-crate" = "crates/my-crate" }
resources = "/absolute/path/outside/repository/factorio-resources"
first_port = 15000
setup = []
teardown = []
```

Module directories must exist, be non-overlapping and contain no symlinks. The host
offers this configured repository during onboarding. It does not accept arbitrary
filesystem paths from clients. One workspace per repository keeps claims and port
allocation repository-wide. Onboarding grants the authenticated creator ownership.
Another identity receives no access unless granted through Access.

Give Authy's registered Factorio client and Factorio's `oauth.client_secret` the
same credential in their respective encrypted bags. Authy's client entry registers
Factorio's origin; `[app.oauth].issuer` pins Authy. Preserve these credentials and
Authy's database when resetting Factorio.

```sh
mise exec -- cargo build -p factorio-native -p factory-cli
target/debug/factorio --config apps/factorio/.deployment/development/config.toml --migrate
./bin/snap dev apps/factorio
```

Startup verifies migrations and leaves an empty database empty. After sign-in,
choose the repository and create the first workspace. The next screen asks what
you want to work on. `[host]` configures native IO and optional `[dev].listen`
selects the frontend listener. Packaged builds
include the server, native `factory` executable, web bindings and OpenCode bridge.

With `[dev].listen = "0.0.0.0:3852"`, `snap dev` accepts this machine's discovered
LAN/Tailscale addresses and Tailscale names. Run Authy in dev mode for matching
callback registration. Keep `[app.oauth].issuer` pinned to Authy's stable URL.
See [network development](../../README.md#network-development) for configuration.
Packaged executables have no source watcher.

## Workspace interface

Intakes, Tickets and Sessions are separate sections. Desktop uses top navigation
and a 280px ticket/session sidebar with independently scrolling details. At widths
of 800px or less, navigation moves to the bottom and the list opens in a native
modal bottom drawer. Selecting a record closes the drawer; Escape and its close
button restore focus to the list trigger. Account holds sign-out and agent-token
actions. Mobile conversations resize to the visual viewport and hide bottom
navigation while the software keyboard takes space.

`/intakes/:id`, `/tickets/:id` and `/sessions/:id` identify selected records.
Section navigation remembers selections within the mounted workspace. Index
routes select the first visible ticket/session, while missing direct links show an
unavailable state. Old root intake/ticket hashes and section anchors redirect to
their corresponding routes.

Ticket/session lists default to open records, excluding done/cancelled tickets and
complete/abandoned sessions. The All filter includes those records. Lists sort by
server-owned `created_at` Unix seconds, newest first, then ID. Editing and lifecycle
changes preserve creation time. Legacy Documents deserialize without a timestamp
and sort last by ID; their original creation dates are not fabricated.

## Documents and transport

Workspace, Ticket, Session and Intake are separate Documents. Workspace indexes
link children, whose Access is inherited from the workspace. Loading includes all
authorized active Documents. The Rust Document client owns browser reconciliation;
the browser and native CLI share guarded operations and retained logical state.
The CLI allocates invocation IDs in a private locked file across process launches.
Browser reattachment refreshes the OAuth access lease through session bootstrap.
An ended login or terminal transport failure settles outstanding calls rather than
leaving the UI waiting. Deleted ticket/intake IDs may be reused, but receive fresh
Document identities and intake conversations; retained receipts stay on old identities.

Browser operations run over `/transport` WebSocket; native `factory` uses the
binary TCP carrier on configurable loopback port 1248. `factorio.command`
composes ticket/session changes in one Store transaction; intake creation, draft
batches, readiness, deletion and onboarding use their own guarded operations.
No HTTP workspace, command, approval, token or intake-tool write endpoint remains.
`GET /api/session` and OAuth routes bootstrap identity. Agent tokens are normal
account credentials tied to the issuing OAuth session; agents cannot issue tokens
or approve candidates. `factory help` documents the CLI and credential-file flags.

## Native CLI

`factory login` prints an approval link and request code. Sign in with Authy if
needed, reopen that link, compare its code with the terminal and allow CLI access.
The CLI acquires its credential over TCP and writes a private file under
`$XDG_CONFIG_HOME/factory` or `~/.config/factory`. `--credentials PATH` selects an
explicit file and permits subsequent calls without shell environment variables.
`factory login --token TOKEN` can instead exchange an existing agent token.
Never put tokens in shared shell history; `FACTORIO_TOKEN` can supply them instead.
Legacy host-managed agent files remain supported for browser intake tools.

The CLI supports repositories/workspaces/onboarding, status, ticket edits/deletion,
start/scope expansion/publication/acceptance/recovery/cleanup/abandonment, intake
creation/read/draft-save/readiness/deletion, and `invoke OPERATION JSON` for any
registered operation. `watch` streams the real Document SDK's replicated view as
JSON lines. Command execution uses no Bun, Node, JS, HTTP or Wasm runtime. Human
candidate approval remains browser-only; `accept` requires that prior approval.

`factory intake -- <description>` can prepare and open a real OpenCode CLI session.
`--no-open` performs only Factorio operations. Direct CLI intake uses OpenCode's
own API/terminal executable, not Factorio's browser relay, and its locally configured
model. Browser intake continues to use the server's explicit model selection.

`[app.tcp]` defaults to `listen = "127.0.0.1:1248"` and
`retention_ms = 1800000`. `FACTORIO_ADDR` or `--addr` overrides the client endpoint;
DNS names and bracketed IPv6 are supported. The listener is TLS-only and requires
PEM `cert_file` and `key_file`. Clients verify the chain and endpoint name before
sending credentials. `--ca-file` selects a private CA bundle instead of public
roots; `--server-name` sets the verification name for tunnels. Their environment
equivalents are `FACTORIO_CA_FILE` and `FACTORIO_SERVER_NAME`. Trust settings are
saved with credentials and cannot change during pending recovery. Server-side
`ca_file` and `server_name` configure generated local intake tool credentials.
`FACTORIO_ORIGIN` is no longer a native transport selector. EOF detaches without
releasing residency. Later processes reuse the saved client ID; logical expiry or
host restart creates a fresh lifetime. An interrupted invocation is saved before
sending. `factory retry` recovers only that exact invocation on a confirmed retained
lifetime with its original opaque identifier, never after expiry/restart or an
unobserved replacement handshake. Successful login explicitly abandons unresolved
recovery state without undoing server commits; failed login preserves the old
endpoint, trust settings and pending invocation. No mutation or credential exchange is
automatically replayed after IO failure.

CLI credentials expire within 30 minutes and never outlive the parent OAuth lease.
They cannot refresh OAuth; run login again after expiry. Logout revokes the current
CLI credential, not the browser session. Native TCP supports remote TLS endpoints
without plaintext fallback. Port 1248 is also used by Hermes and
is configurable to avoid conflicts.
Logical TCP results and Document updates can span bounded 64 KiB physical frames,
up to 16 MiB per logical message. Upgrade client and server together.

## Intake and tickets

OpenCode owns conversation history, questions and agent execution. Factorio's
authenticated proxy forwards conversation actions and streams snapshots outside
the application gate, so an agent can call back into Factorio without deadlocking.
The proxy checks workspace Access. Service credentials remain server-side.

The agent receives the [intake guide](INTAKE.md) and absolute CLI commands. The CLI
reads a private account credential file under the workspace's resource directory;
credentials are never embedded in prompts. There are no intake-only credentials.
Reconnect provisions a current account token if the browser session changed.

Draft batches carry a revision and commit atomically. A stale revision or invalid
ticket rejects the batch. Multi-module work becomes draft parents and single-module
leaves with explicit blockers. The user can grill a draft, edit its details, or mark
implementation leaves ready after reviewing the agent's recommendation. Starting a
ready ticket allocates its worktree and OpenCode session; resume that session to
perform the implementation. Deleting an intake preserves its tickets.

## Session controllers

Commands commit desired state and claims before external IO. One Session controller
performs one effect per pass under the shared application gate, then publishes its
observation. Startup scans resume unfinished non-blocked work. Errors retain desired
state, resource claims and visible blocked status until explicit recovery.

Setup records the repository base, prepares the worktree, runs setup hooks and
creates or moves a stable OpenCode session. Publication checks clean work and module
scope, then records the candidate and mainline OIDs with evidence. An authenticated
human must approve that exact candidate before acceptance. Integration prepares and
commits its exact merge OID before moving mainline. Restart checks ancestry of that
OID instead of merging twice. Ticket completion and the session's merge observation
commit atomically. Candidate or mainline movement requires new publication/approval.

Cleanup retains claims and finalizers until teardown succeeds. Deleted or archived
Sessions remain available to cleanup. Only clean merged worktrees are removed;
abandonment preserves unmerged work. Data directories and branches remain for
inspection. No push, deployment or external tracker publication is automatic.

## Hooks and OpenCode

Setup/teardown argv receive `PORT`, `SNAP_DATABASE`, `FACTORIO_DATA`,
`FACTORIO_SESSION` and `FACTORIO_REPOSITORY`. Hooks are idempotent, finish within
120 seconds, explicitly migrate their own database and leave no detached services.
The host owns their process groups and records PID/birth markers for crash recovery.

Use OpenCode V2. `[app.tools].opencode` selects its CLI; `bun` selects Bun.
The bridge uses the official client and local service discovery/authentication.
Factorio selects `opencode-go/muse-spark-1.3-contributor` explicitly for intake
conversations and ticket session setup. `[app.tools].model = "provider/model"`
overrides selection, including retained conversations on reconnect or reply.
The reply footer shows the session model reported by OpenCode, including its provider
and variant when present. Selection failures stop submission rather than falling back
to the global default.
`[app.tools].bridge` supports installations and isolated fixtures. See
[configuration](../../docs/configuration.md) for packaging and secrets. Factorio does not
change global OpenCode configuration or wait for an agent turn under its gate.
