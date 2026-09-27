# Access, Document and application upgrade brief

## Purpose and execution status

This is the user's requested working brief for the upcoming unattended implementation
session, expected to last roughly five hours. It records the agreed scope before the
user supplies an additional application project. Writing this brief does not start
the implementation run. Incorporate the next project before beginning that run.

Work through the sequence below to completion, verifying behavior as it lands.
Elapsed time is not evidence of completion. Report unfinished work and blockers
honestly. Keep this brief's status current during execution; maintain architectural
decisions and operating instructions in their existing authoritative documents.

## Sources and starting state

- Current Rust repository: `~/code/snapco/snap`.
- For unattended execution, filesystem work is restricted to this repository and
  `/tmp/opencode`. Use staged references in
  `/tmp/opencode/snap-upgrade-references/old-snap-packages` and `old-factory` instead
  of accessing the original paths below. These are source working-tree copies,
  excluding Git metadata, dependency/build directories, environment files and symlinks.
  The existing integration is a command, not a skill. Its content is staged as
  `/tmp/opencode/snap-upgrade-references/factory-command.md`, copied from the
  user-identified `~/.config/opencode/commands/factory.md`. Create the replacement
  command repo-locally; do not search or modify global configuration. Its references
  to Linear and Origin are legacy behavior, not requirements for the new app.
- Original implementation: `~/code/bod/snap`, particularly
  `packages/core/src/access`, `packages/engine/src/acl`,
  `packages/core/src/document`, and `packages/engine/src/snapshot`.
- Read `AGENTS.md`, `README.md`, `ARCHITECTURE.md`, `TESTING.md`,
  `docs/store.md` and `docs/identity.md` before relevant implementation work.
- Existing Authy and Chatty contracts describe behavior worth preserving, but also
  contain obsolete architecture and wire details. Reconcile them with this brief;
  do not treat legacy compatibility requirements as binding.
- Access and Document are not implemented in this workspace. Store, transport and
  Identity are implemented. Authy and Chatty are excluded pending rewrites.
- Dev/build restoration and Testy hot reload are implemented and verified, included
  in the committed handoff baseline. Extend usable workflows to the upgraded apps.
- The committed baseline also preserves earlier Authy contract, configuration and
  test work, Chatty configuration, and Authy browser/integration/SDK/support tests.
  These excluded apps still require their planned ports; committing their existing
  tests does not make the legacy implementations runnable. Inspect git status before
  editing and preserve any newer work that appeared after this handoff.
- `docs/bend-language-research.md` is unrelated existing work to preserve.

## Global rules

- Replace obsolete systems. Do not keep compatibility shims, parallel legacy
  runtimes, dead providers or outdated contracts merely to avoid rewriting callers.
- Preserve useful behavior, including development commands and hot reload.
- Portable capabilities stay `no_std` with `alloc`. Hosts own execution, clocks,
  randomness, scheduling and external IO. Portable code is IO-free.
- Store remains a low-level server-side capability, independent of Identity,
  Access, Document and transport. Apps may use Store and transport directly.
- Cross-module writes share one caller-owned Store transaction. Durable commit
  precedes publication, completion and external effects.
- Store misses terminate the attempt and discard staged writes. No implicit load,
  suspension or automatic retry. Hosts explicitly arrange residency.
- Initially keep server operation dispatch globally serialized. Do not introduce
  concurrent mutation execution while porting.
- No publication or remote workflow is required. The repository is local-only.

## 1. Port Access onto Store

Status: not started.

Preserve the original authorization semantics while removing its redundant storage,
residency, locking and transaction-publication infrastructure.

- Declare Store tables and indexes for resources, direct grants and parent/child links.
- Preserve resource kinds, audience policy, viewer/editor/manager/owner roles,
  strongest-role inheritance, cycle prevention, ownership transfer and link authority.
- Keep audience-derived viewing distinct from authority used for ownership/link edits.
- Expose synchronous resource registration, grant/link changes, role resolution,
  accessible-resource enumeration and minimum-role checks through Store transactions.
- Check authority against the appropriate pre-change state. A mutation cannot grant
  itself the authority needed to perform that same change.
- Access changes and caller-owned document changes commit atomically. Publish any
  derived invalidations only after commit. Failed transactions publish nothing.
- Keep Access independent of Document so other modules can protect resources.
- Start with simple correct graph evaluation; optimize only when justified.

Done when meaningful tests cover inheritance, multiple paths, revocation, cycles,
audiences, transfers, authority checks and rollback with another module's writes.

## 2. Port Document onto Store and Access

Status: not started. Depends on Access.

### Ownership and loading

- Document owns definitions, deterministic mutations, document guards, revisions,
  its transport protocol, client SDK and replication/reconciliation behavior.
- Store owns resident rows, indexes, isolation and durability. Do not duplicate the
  old Snapshot persistence coordinator or writer locks.
- Access provides authorization and the loading manifest. If a client may read a
  document, load the entire document. No subset/extent interest policy in this port.
- Grant changes add or remove desired client holdings. Revocation stops subsequent
  authorized writes and delivery, including work waiting to be delivered.
- Client removal cannot retract data already received.
- Server and client should share canonical in-memory representations and mutation
  behavior as closely as possible. The server holds many identities' data; each
  client holds its authorized data. Literal cross-target binary layout equality is
  not promised.
- Connection-owned references are intended to retain server data, while the client
  holds its own references. Current Store has no eviction/refcount residency system;
  do not claim that it already provides this. Keep lifetime ownership explicit
  without inventing a speculative eviction system.

### Mutation and optimistic journal

- `document.mutate` invokes one named mutation with arguments on one document.
- Multi-document changes require an app-owned transport operation composing module
  calls in one Store transaction. No generic multi-document optimistic operation.
- The visible client view is authoritative state with an ordered journal of pending
  mutation intents replayed over it.
- Local edits enter that journal immediately. The SDK sends one submission at a time,
  allowing the next after acceptance ACK, not after completion. Server execution
  remains serialized. ACK confirms acceptance, not durability or success.
- Existing carriers allow one outstanding command per physical connection. Integrate
  ACK-paced submission deliberately across transport, host and SDK; an SDK queue alone
  cannot implement the intended lifecycle.
- Other replicas receive ordered intents. The originator receives authoritative
  effects in completion instead of ordinary replication of its own mutation.
- Apply completion effects and remove that mutation's journal entry as one coherent
  client publication. Replay remaining entries without losing or double-applying them.
- Notify reactive consumers on local journal changes, completions and replication.
- Surface rejections and reconcile remaining optimistic work explicitly.
- Initial journal entries can contain ordinary mutation names and arguments.
  Intent-replication bytecode, persistent journals and offline support are future
  work. Shape the interfaces to support them without designing bytecode now.
- Conflict policy is deliberately provisional. Choose a simple correctness-first
  policy and document it. Blanket stale-base rejection was discussed but not settled;
  it must not break causal ordering of ACK-paced optimistic edits. More sophisticated
  per-mutation conflict controls can wait.

### Replication and recovery

- Intent application requires a compatible mutation implementation and matching
  authoritative base. Divergence is an observable error, followed by recovery,
  never a silently accepted normal condition.
- `document.manifest` presents current holdings. The server chooses catch-up or
  authoritative replacement based on available history, compatibility and cost.
  Do not require replaying every intermediate change when replacement is simpler.
- Client state is ephemeral initially. Reconnect on a surviving logical connection
  can present retained state; a fresh/reset client presents an empty manifest.
- Persist mutation receipts atomically with writes to recover an interrupted result
  without executing the mutation twice while the logical connection survives.
  Do not reproduce the old cached-receipt crash gap.
- Logical-connection expiry clears client authoritative/optimistic state and pending
  work, then rebuilds through a fresh manifest. Never replay expired-lifetime writes.
  Reset does not undo writes already committed on the server.
- Current transport does not implement durable result recovery. Add the required
  protocol rather than assuming invocation IDs already provide deduplication.
- Future offline persistence will require revisiting connection-expiry journal loss.

Done when the client/server slice demonstrates guarded mutation, optimistic updates,
ACK/completion separation, two-client replication, revocation, reconnect recovery,
expiry reset, receipt deduplication, mismatch reporting/recovery and reactive views.
Use deterministic/property testing for the interacting state machines as appropriate.

## 3. Upgrade Authy

Status: not started. Depends on Access and Document.

- Make Authy runnable on current transport, Store and Identity, with Access-protected
  Document profiles and the Document client SDK for profile interaction.
- Identity owns credentials and sessions. Do not restore Passport.
- Create identity, first session, initial profile and required grants atomically.
- Preserve account/profile behavior and standard OAuth 2.0 / OIDC issuer functionality.
  OIDC owns specialized Store tables and standard HTTP endpoints, not Document rows
  exposed to clients as a substitute for its protocol.
- Preserve code flow with PKCE, explicit consent, registered redirects, token signing,
  refresh rotation/replay handling, grant revocation and login/logout behavior.
  Use the existing contract and implementation to enumerate the complete behavior.
- Preserve durable credentials, sessions and signing identity across rebuild/restart.
- Keep secrets and bearer material out of retained diagnostics and client views.
- Replace obsolete compositions, config, SDK wiring and tests rather than adapting
  new code to legacy wire compatibility.
- Restore app-local and root-invoked `snap dev` / `snap build`, including frontend
  refresh and native rebuild behavior. Config parsing alone is not completion.

Done when account creation, login, profile edits, reconnect, logout and OIDC code/
refresh/revocation flows work through real hosts and browser journeys.

## 4. First code and documentation cleanup

Status: not started. Follows Authy upgrade.

- Remove superseded implementations, unused dependencies, old configuration paths,
  obsolete generated bindings and tests that only assert deleted behavior.
- Update authoritative architecture, identity/store documentation, app contracts,
  setup commands and testing instructions to describe verified current behavior.
- Keep historical implementation details out of active contracts. No claim of a
  working host or deployment without a runnable implementation and verification.
- Inspect legacy native/Workers compositions. Port required working targets or remove
  obsolete targets and their claims explicitly; do not leave broken excluded code
  presented as a supported implementation.
- Preserve useful dev/build tooling and unrelated user work during deletion.

## 5. Upgrade Chatty, using only Authy OAuth for login

Status: not started. Depends on upgraded Authy and shared capabilities.

- Replace all old runtime/storage/authentication compositions with current systems.
- Authy OAuth/OIDC is Chatty's sole login path. No local password registration, direct
  Authy database access or shortcut bearer sharing between apps.
- Preserve authorization-code/PKCE flow and issuer, signature, audience, nonce,
  redirect and browser-correlation checks. Keep OAuth tokens server-side.
- Derive stable ownership from issuer plus subject. A Chatty-local session derived
  from OAuth is permitted; it is not an independent credential/login system.
- Use Access and Document for client-visible private conversation state where they
  fit. Keep server-only tokens and provider/internal data in app-owned Store tables.
- Preserve persistent threads, generation progress, cancellation, request deduplication,
  ownership isolation and the existing useful model/tool behavior.
- Keep external model/tool IO host-owned and outside Store transactions. Persist
  acceptance before starting effects and fence late progress after cancellation,
  logout or deletion. Never automatically replay uncertain external effects.
- Preserve the rule that opaque provider reasoning and credentials do not enter
  client-visible documents or replication.
- Make dev/build and the Authy+Chatty local workflow usable with documented configuration.

Done when a browser can log into Authy through Chatty, return to Chatty, create and
use private persistent threads, observe progress, reconnect and log out. Verify
cross-account isolation and token/session lifecycle. Use deterministic provider
fixtures for repeatable tests; if real provider credentials are unavailable, report
that specific live verification as blocked rather than claiming it passed.

## 6. Final cleanup and verification

Status: not started. Follows Chatty upgrade.

- Repeat code/dependency/config cleanup across the entire migrated workspace.
- Prune stale concepts and misleading commands from all maintained documentation.
- Ensure no excluded legacy implementation remains merely as a fallback.
- Verify fresh setup, explicit migrations, build, dev startup, reload and orderly
  shutdown for supported apps. Never make application startup silently migrate.
- Follow `TESTING.md`; run relevant Rust tests, portable-target checks, Clippy,
  TypeScript/build checks, meaningful property suites and browser/integration journeys.
- After package/dependency changes run:
  `mise exec -- ./bin/snap check apps/testy --structure-only --workspace`.
- Re-run Testy's meaningful gates so adding these capabilities does not regress its
  authenticated calculators, connection isolation or development workflow.
- Verify failures as well as success: failed commits publish nothing, denied access
  leaks no document data, interrupted results do not duplicate writes, and failed
  rebuilds retain the previous usable generation.
- Finish with a concise report of completed work, actual checks/results, remaining
  blockers, operational commands and any decisions still requiring user input.

## 7. Build Factorio in `apps/factorio`

Status: not started. Depends on the upgraded shared capabilities.

Factorio is a Snap application for local software development, likely to ship with
Snap and remaining in this monorepo. It runs alongside OpenCode V2 on the same host
OS. Its first version supports manually driven parallel agent development, not a
fully automated factory. GitHub, Linear, PRs, Buildkite and public deployment are
not dependencies of its workflow.

Inspect the staged old factory and factory command for workflow context.
They are references, not implementations
to copy unquestioningly. Read the OpenCode skill and current V2 API/client docs
before implementing its integration; do not guess endpoints or use V1 contracts.

### Tickets

- Provide CRUD for title, description, target module(s), status and progress notes.
- Support optional parent/child relationships and explicit blocker edges. Reject
  cycles and render a basic dependency graph or equivalent navigable dependency view.
- A ticket is actionable when marked ready and all blockers are resolved.
- Link tickets to implementation sessions and expose claim/progress state.
- Persist tickets inside the Snap application, using Store, Access and Document
  as appropriate. External trackers are not the authority.
- Ticket sources, automatic decomposition, production migrations and automated
  feedback loops are future work; do not build a general workflow language now.

### Sessions and module exclusion

- A Factorio session is an implementation attempt that claims work, owns a branch
  and worktree, and leads to review and mainline integration. It is distinct from an
  OpenCode conversation; associate the relevant OpenCode session IDs with it.
- Support ticket-backed sessions and ticketless one-off sessions started with a prompt.
- Declare every module the work intends to change. Active sessions hold exclusive
  claims on those modules. Acquire multi-module claims atomically so overlapping
  work waits or fails clearly rather than creating partial claims or deadlocks.
- Disjoint module work can proceed in parallel. Module names should come from a
  clear repository configuration; cross-cutting work must declare its whole scope.
- Rust crates are the default exclusive module units. A repository-wide claim
  conflicts with every crate claim and covers repository-wide changes; acquire it
  through the same exclusion mechanism, not as an unrelated lock name.
- Ticketless work also needs a declared scope. Scope expansion must acquire any
  additional claims before editing those modules.
- Claims must survive server restart and have explicit release/abandon behavior.
  Do not release a live claim merely because an agent disconnects.

### Start, publish, accept

- Start claims the work/modules, creates a worktree and branch, allocates isolated
  resources, and prepares or moves the OpenCode conversation into the worktree.
- Apps declare their setup requirements. Isolate ports, databases, data directories
  and owned processes. Record allocations and provide setup/teardown hooks without
  inventing a general orchestration platform.
- Publish records an immutable candidate commit and its check/review evidence,
  making it ready for human inspection. No external PR is needed.
- Acceptance requires explicit human approval. Agents may implement and publish;
  they must not approve their own changes as the human. Automated review and staging
  gates can be added later. Track review findings and their dispositions.
- Bind approval and evidence to the candidate being accepted; new commits invalidate
  approval of the earlier candidate. Do not silently accept a different change.
- Accept integrates locally into the configured mainline and releases owned resources
  and module claims after successful integration. It never deploys or publishes software.
- Successful merge deterministically completes the session and its claimed tickets.
  Publish or approval alone does not complete them. Record the integrated commit and
  reconcile a crash between Git integration and Store publication so recovery completes
  the same session/tickets without merging twice. Update the session and claimed
  tickets together in one Store transaction; dependent readiness follows that state.
- Serialize mainline integration even for disjoint sessions. Handle target movement
  explicitly and stop on conflicts rather than silently resolving or losing work.
- Provide cancel/abandon and recovery for partial setup, failed publish/integration,
  process loss and interrupted cleanup. Keep evidence and dirty work recoverable;
  never force-delete an unmerged dirty worktree as incidental cleanup.
- Git, process and OpenCode operations are external effects, not Store transactions.
  Persist lifecycle intent/progress and reconcile interrupted effects rather than
  claiming atomic commit across SQLite, Git and OpenCode.

### Interfaces

- A CLI and minimal browser UI use the same Factorio client SDK and host server.
  Keep business rules in the shared application, not duplicated in interface code.
- CLI supports ticket management and session start/publish/accept/inspection/cleanup.
- Supply a repo-local OpenCode V2 factory command for the CLI workflow, including
  ticketless start and moving the conversation to the prepared worktree. Use the
  staged command as reference and verify current V2 command syntax before writing it.
- Web UI lists and edits tickets, shows blockers, and displays/manages session state,
  module claims, candidate commits and review evidence. Support explicit human acceptance.
- Integrate with OpenCode rather than duplicating its conversation UI. Make associated
  conversations discoverable/openable for human debugging and steering.
- Use Authy OAuth/OIDC for login, like Chatty, with one shared factory workspace
  initially. Reuse Snap Identity/Access/Document facilities rather than a separate
  application state system. Human approval must be attributable to an authenticated
  human action, distinct from agent operations.

### Completion gate

Demonstrate creating tickets with a blocker, starting isolated sessions for disjoint
modules, rejecting overlapping claims, implementing and publishing a change, human
acceptance into mainline, and the dependent ticket becoming actionable. Show the same
records through CLI and browser UI, and exercise restart/failed-setup recovery.

Use disposable repositories for automated acceptance tests. During unattended work,
leave real candidate changes awaiting human acceptance; fixture approval must not
be represented as the user's approval of production work.

Confirmed initial policies: Authy OAuth login, one shared workspace, crate-level
exclusive claims with repository-wide exclusion, and deterministic completion of
the session and its claimed tickets after successful merge.
