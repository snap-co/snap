# Host-driven module reference refactor review

Status: implementation complete; repository verification and independent review
pending. This is a new scope authorized by the user on 2026-09-24. Earlier Authy
and tooling review budgets remain closed.

## Contract and scope

- [Accepted architecture](../adr/0001-host-driven-capability-providers.md).
- [Implementation plan](../plans/host-driven-module-pattern.md) and
  [porting guide](../architecture/module-pattern.md).
- [Store's bounded guarantees](../architecture/store.md).
- [Authy compatibility](../plans/authy-password-sessions.md), with the ADR
  superseding its old synchronous-stage and Lane implementation choices.
- Work remains on `main`. No remote, tracker, new branch, or worktree is involved.

Review base is the design/prototype commit `ee98a70` immediately before the product
refactor. Record full base and reviewed HEAD before reviewer dispatch.

## Implemented decisions

- Protocol, Identity, and Store are peer contract crates. `snap check` enforces
  contract/provider dependency direction across the existing target/feature scope.
- Native and browser web codecs/bindings share `snap-web`. Shared clients receive
  normalized results, events and disconnect reasons. Operations have no Lane.
- Protocol's associated-context/output Provider creates owned continuations.
  The host's scheduler admits and polls them without Passport-specific work types.
  Application composition projects provider output into web delivery effects.
- Store supports declared typed columns, primary/unique/foreign constraints,
  indexed predicates, ordered bounded reads, guarded atomic transactions, updates,
  deletes, and coherent cross-namespace reads. Memory and SQLite share assertions.
- Schema registration migrates Authy's original SQLite names transactionally and
  preserves its records and signing key. Subsequent incompatible schemas fail
  without a silent upgrade. No generalized migration language is claimed.
- Passport uses Store and host crypto through portable async code. Authy chooses
  NoCache. Advisory cache snapshots never replace transaction authority.
- The experiment is archived in `ee98a70`; production interfaces and consumer
  contracts replace its throwaway executable and visualization sources.

## Verification collected before full gate

- Workspace/all-target compilation and warnings-denied Clippy passed.
- Bare-WASM structural checks passed after adding the initial packages; the full
  gate will rerun them with the final contract role.
- Shared Store assertions passed through Memory and SQLite, including separate
  SQLite connections, rollback, uniqueness, foreign keys, coherent reads and cache
  versus authority.
- The carrier contract passed with one operation available through HTTP and
  WebSocket, plus continued accepted work after observer cancellation.
- Authy's wire flow, original-database migration, eight native/browser recovery
  cases, and TypeScript checking passed in focused runs.

## Development process note

The existing live dev watcher was still running during early edits. It correctly
retained working generations on compile failures, then ran an intermediate build
that renamed the user's local database and installed a preliminary Store registry.
When that registry's shape changed during development, startup rejected it and dev
restored its previous executable. The coordinator stopped that owned dev process,
made a mode-0600 SQLite backup at
`/tmp/opencode/authy-before-store-finalization.sqlite`, and removed only the
intermediate `snap_store_schemas` registry. Accounts, sessions and signing-key rows
were preserved; final startup revalidates and registers the existing tables.

No production fallback accepts arbitrary mismatched schema fingerprints. The
fixture migration contract independently starts from the original pre-refactor
schema. Future persistence refactors should stop the live watcher before changing
schema registration; exercise migrations on owned fixtures before restarting it.

## Review rounds

Zero rounds used. Both Standards and Spec axes are required in round 1; at most
one post-fix validation round may follow, under implementation-review.
