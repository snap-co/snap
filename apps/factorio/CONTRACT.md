# Factorio

Factorio coordinates one local Git repository through a shared Authy-authenticated
workspace. The portable application owns tickets, blocker/parent graphs, exclusive
claims, candidate evidence, approval and lifecycle transitions. The native Linux
host owns Git, finite setup/teardown hooks and OpenCode V2 operations.

## Setup

Create a JSON configuration and set `FACTORIO_CONFIG` to its absolute path:

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

Use Cargo package names and their repository-relative directories for modules.
`*` is the repository-wide claim and conflicts with every crate. Declare all
cross-cutting work before editing. The configuration is pinned in the workspace;
changing it requires an explicit future migration, rather than silently redirecting
existing worktrees. The main checkout must be clean and on the configured mainline
for integration. Resources live outside that checkout.

Set the same randomly generated `FACTORIO_CLIENT_SECRET` of at least 32 bytes on
Authy and Factorio. Authy registers Factorio when this variable is present.
`FACTORIO_ORIGIN` on Authy defaults to `http://127.0.0.1:3852`. Factorio uses
`AUTHY_ORIGIN`, defaulting to `http://127.0.0.1:3846`. Start Authy using its existing
development instructions.

```sh
mise exec -- cargo build -p factorio-native
SNAP_DATABASE=apps/factorio/.snap/factorio-store.sqlite target/debug/factorio --migrate
./bin/snap dev apps/factorio
```

Startup opens explicitly migrated Store tables. It never migrates automatically.
`SNAP_DATABASE`, `SNAP_ORIGIN`, `SNAP_WEB_DIR` and `FACTORIO_ADDR` override native
defaults. `FACTORIO_WEB_ADDR` selects the public dev address. Build with
`./bin/snap build apps/factorio`; the package is `dist/factorio/factorio` plus `web`.
Packaged launch selects the adjacent `web` directory. Retain the same database,
configuration and OAuth environment.

## Work

Sign in through the browser, then create an agent token and export it as
`FACTORIO_TOKEN` in the CLI environment. Tokens are stored as digests and refer to
the authenticated OAuth session. Logout/expiry ends their authority. Keep the
browser session active to renew upstream access. Agent tokens cannot mint other
tokens or approve candidates.

`bin/factory help` lists CLI syntax. Ticket input is a JSON file containing `id`,
`title`, `description`, `modules`, `status`, `notes`, `parent` and `blockers`.
Statuses are `draft`, `ready`, `cancelled` and host-completed `done`. A ready ticket
can start only after all blockers are done. Deleting referenced tickets fails.

```sh
bin/factory start --id fix-parser --modules parser --tickets parser-ticket -- Fix parser
bin/factory publish fix-parser --evidence /path/to/check-and-review-evidence.txt
bin/factory accept fix-parser
```

Start commits all claims and resource allocations before setup. Failed setup retains
the claims, worktree, data and error. Inspect `status`, repair the setup cause and
run `recover <id>`. Disconnect never releases a claim. `expand <id> --modules ...`
acquires additional scope atomically while active. Publication rejects changes
outside the declared directories. Exclusion coordinates cooperative agents; it is
not a filesystem sandbox against a local process intentionally editing other paths.

Publish requires a clean worktree and records the exact candidate OID, mainline OID,
evidence and findings/dispositions. The browser exposes an explicit confirmation
for the authenticated human. Approval cannot be supplied in a CLI command or JSON
actor field. The host attributes approval to the OAuth identity. This separates
agent credentials from human browser actions; it does not prove physical human
presence against software controlling that browser.

`accept` requires that approval, checks both recorded OIDs, constructs a merge commit
without changing the worktree, then durably records its OID before fast-forwarding
mainline. Candidate or target movement requires a new publication and approval.
Conflicts stop for inspection. Mainline integration is serialized across sessions.
No remote push, PR, deployment or external tracker is involved.

Recovery checks whether mainline contains the exact planned merge commit. It then
completes all claimed tickets in the same Store transaction as the session's merge
transition. A dependent becomes actionable from that committed state. Cleanup
retains claims until successful teardown. It removes only a clean, merged worktree.
Dirty or unmerged work is preserved with an actionable error. Data directories and
branches remain for inspection; deletion of stored development data is explicit.
`abandon` runs teardown, releases claims and preserves the unmerged worktree.

## Resources and hooks

Each attempt receives a persisted unique port, worktree, branch and data directory.
The host checks port availability during setup. Hook argv runs directly with
`PORT`, `SNAP_DATABASE`, `FACTORIO_DATA`, `FACTORIO_SESSION` and
`FACTORIO_REPOSITORY`. Hook setup must be idempotent and explicitly migrate its own
database. Hooks finish within 120 seconds; they may not leave detached services.
The host owns each hook process group. A durable PID/birth record and pre-execution
handshake allow restart to retire an interrupted group without killing a reused PID.
Interrupted setup requires explicit recovery; committed merge/cleanup is reconciled
at startup. Shell history, manual dev processes and their shutdown remain the
operator's responsibility.

## OpenCode V2

Install a V2 CLI that supports `opencode api`. Set `FACTORIO_OPENCODE` to its path
when the default executable is another version. The adapter uses that CLI's service
discovery/authentication, `POST /api/session` with a stable supplied ID and location,
and `POST /api/session/{id}/move`. It never reads or modifies global configuration.
Pass an existing `--conversation ses_...` to associate and move that conversation.
The command in `.opencode/commands/factory.md` guides ticket-backed and ticketless
work and confirms the harness directory before editing.

The browser displays the associated conversation ID and a CLI resume command.
OpenCode owns conversation display and steering. The server integration is based
on the [V2 API](https://opencode.ai/v2/docs/api), its OpenAPI schema and the
[V2 command documentation](https://opencode.ai/v2/docs/commands).

## Verification

```sh
mise exec -- cargo test -p factorio
TMPDIR=/tmp/opencode mise exec -- cargo test -p factorio-native -- --include-ignored
TMPDIR=/tmp/opencode mise exec -- bun scripts/test-factorio.ts
TMPDIR=/tmp/opencode mise exec -- bun scripts/test-factorio.ts --dev
```

Core tests own graph, authority, claim rollback and atomic completion guarantees.
Native tests own real Git movement, scope checks, merge reconciliation and dirty
work preservation. The browser journey uses real Authy OAuth, Store, Document,
CLI and disposable Git repositories. OpenCode is an explicit V2 contract fixture
in that journey. Automated approval applies only to disposable fixture code and
never constitutes the user's acceptance of a real candidate.
