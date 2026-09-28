# Factorio

Factorio turns an OpenCode intake conversation into draft tickets, then coordinates
ticket-backed work and human-approved local integration. Authy owns sign-in.

## Setup and onboarding

`FACTORIO_CONFIG` names a JSON file describing an existing server-hosted repository:

```json
{
  "repository": "/absolute/path/to/repository",
  "mainline": "main",
  "modules": { "my-crate": "crates/my-crate" },
  "resources": "/absolute/path/outside/repository/factorio-resources",
  "first_port": 15000,
  "setup": [],
  "teardown": []
}
```

Module directories must exist, be non-overlapping and contain no symlinks. The host
offers this configured repository during onboarding. It does not accept arbitrary
filesystem paths from clients. One workspace per repository keeps claims and port
allocation repository-wide. Onboarding grants the authenticated creator ownership.
Another identity receives no access unless granted through Access.

Set the same `FACTORIO_CLIENT_SECRET` on Authy and Factorio. Authy uses
`FACTORIO_ORIGIN` to register the callback. Factorio uses `AUTHY_ORIGIN` to reach
Authy. Preserve these credentials and Authy's database when resetting Factorio.

```sh
mise exec -- cargo build -p factorio-native
SNAP_DATABASE=apps/factorio/.snap/factorio-store.sqlite target/debug/factorio --migrate
./bin/snap dev apps/factorio
```

Startup verifies migrations and leaves an empty database empty. After sign-in,
choose the repository and create the first workspace. The next screen asks what
you want to work on. `SNAP_DATABASE`, `SNAP_ORIGIN`, `SNAP_WEB_DIR`, `FACTORIO_ADDR`
and `FACTORIO_WEB_ADDR` retain their usual native/dev overrides. Packaged builds
include the executable, web bindings and OpenCode bridge.

## Documents and transport

Workspace, Ticket, Session and Intake are separate Documents. Workspace indexes
link children, whose Access is inherited from the workspace. Loading includes all
authorized active Documents. The Rust Document client owns browser reconciliation;
the browser and CLI share invocation ACK, same-ID retry and reconnect recovery.

Live application operations run over `/transport` WebSocket. `factorio.command`
composes ticket/session changes in one Store transaction; intake creation, draft
batches, readiness, deletion and onboarding use their own guarded operations.
No HTTP workspace, command, approval, token or intake-tool write endpoint remains.
`GET /api/session` and OAuth routes bootstrap identity. Agent tokens are normal
account credentials tied to the issuing OAuth session; agents cannot issue tokens
or approve candidates. `factory help` documents the CLI and credential-file flags.

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

Use OpenCode V2. `FACTORIO_OPENCODE` selects its CLI; `FACTORIO_BUN` selects Bun.
The bridge uses the official client and local service discovery/authentication.
`FACTORIO_INTAKE_MODEL=provider/model` optionally selects new intake models.
`FACTORIO_OPENCODE_BRIDGE` supports isolated contract fixtures. Factorio does not
change global OpenCode configuration or wait for an agent turn under its gate.
