# Host-driven module reference refactor review

Status: READY. Repository verification and both independent review axes passed.
This is a new scope authorized by the user on 2026-09-24. Earlier Authy
and tooling review budgets remain closed.

## Contract and scope

- [Accepted architecture](../adr/0001-host-driven-capability-providers.md).
- [Implementation plan](../plans/host-driven-module-pattern.md) and
  [porting guide](../architecture/module-pattern.md).
- [Store's bounded guarantees](../architecture/store.md).
- [Authy compatibility](../plans/authy-password-sessions.md), with the ADR
  superseding its old synchronous-stage and Lane implementation choices.
- Work remains on `main`. No remote, tracker, new branch, or worktree is involved.

Immutable review base: `ee98a70afac25a3caee8a0306db633efb1ab18c1`.
Round-1 HEAD: `a47a4018c7755c548eeb4d3bac55edc7ca72afe6`.
Commit: `Implement host-driven providers, shared Store and carrier bindings`.
Captured diff: `/tmp/opencode/module-pattern-round1.diff`.

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

Round 1 is dispatched below for independent Standards and Spec assessments of
the same immutable revision and contract. At most one post-fix validation round
may follow, under implementation-review.

Model discovery confirms `openai/gpt-6-astra` and its high/xhigh variants. Persistence
migration and continuation lifetime warrant deeper review. The tool's model field
requires an explicit user model request; none was made, so the reviewers use the
harness's default Astra rather than an unauthorized variant override.

The full `mise exec -- ./bin/check` passed on the recorded HEAD. Log:
`/tmp/opencode/module-pattern-full-check.log`. It includes format/Clippy, all-target
builds, final contract-role/bare-WASM structural checks, Rustdoc, dependency policy,
CLI suites, Store contracts, Healthy SDK/wire/journey/reference, Authy wire/migration,
carrier/lifecycle, eight recovery cases, TypeScript interoperability, all nine
Chromium scenarios, and listener replacement/cleanup.

`mise exec -- ./bin/snap check apps/authy` also passed on the recorded HEAD, including
its newly wired Store/carrier/migration commands from the application directory.
Log: `/tmp/opencode/module-pattern-project-check.log`. Reviewers may run focused
isolated probes but must leave shared builds/full gates to the coordinator.

- Standards session: `ses_f2a230405ffewKza60BDz1ajjN`.
- Spec session: `ses_f2a228380ffeuUqadFQn8PYy57`.

## Round-1 findings and repair disposition

Both axes returned BLOCKED. Complete reports are archived alongside this ledger:
[Standards round 1](host-driven-module-pattern-standards-round1.md) and
[Spec round 1](host-driven-module-pattern-spec-round1.md).

| Finding | Disposition | Repair |
| --- | --- | --- |
| STD-1 / SPEC-1 | Accepted, duplicate namespace-isolation blocker | Both backends reject identifiers outside lowercase ASCII letters, digits and underscores. Persisted SQLite ownership lookups use NOCASE. Shared registration tests cover case aliases, reserved/legacy names, rejection on reopening and preserved rows. |
| STD-2 | Accepted carrier-boundary blocker | Core uses opaque operation IDs and an attachment flag. Each native/browser physical connection owns a snap-web codec, sequences web IDs and maps completions back to controller IDs. Generation fencing and deadlines still use core IDs. |
| SPEC-2 | Accepted HTTPS compatibility blocker | Native composition and web host share parsed origin validation. Cookie policy uses the normalized scheme. Wire regression verifies uppercase HTTPS yields a Secure __Host cookie that survives restart. |
| SPEC-3 | Accepted as in-scope repair | This cache-enabled failure was introduced by this refactor's optional Passport cache support, so it is repaired in the same batch rather than filed as independent work. Empty snapshots reload authority before rejecting login. One wire sequence runs through NoCache and a MemoryCache composition. |

No requirement or threat model was broadened. No independent finding remains to
file; this repository has no configured tracker. Authy's native projection moved
to `src/lib.rs` so packaged and cache-enabled launchers share composition. The
packaged executable still selects NoCache.

Initial repair gate stopped at cargo-machete: moving socket serialization to
snap-web left snap-browser's serde_json dependency unused. Removed that dependency
before the final validation revision. No behavior check had failed.

## Round 2: final fix validation

Prior reviewed HEAD: `a47a4018c7755c548eeb4d3bac55edc7ca72afe6`.
Validation HEAD: `72954a3dbd0e27b2400e186067f176c4f22d1c21`.
Repair commits: `a864b932753c6be7f6be7b73864e202cace9cfb0` and the dependency cleanup.
Fix delta: `/tmp/opencode/module-pattern-round2-fixes.diff`.

Round 2 is the only post-fix validation. Both original reviewers resume against
this fixed revision and their findings plus affected interactions. No new whole-diff
review is authorized. The agreed contract and exclusions are unchanged.

Verification on the repaired code:

- `mise exec -- ./bin/check` passed. Log:
  `/tmp/opencode/module-pattern-final-full-check.log`.
- `mise exec -- ./bin/snap check apps/authy` passed. Log:
  `/tmp/opencode/module-pattern-final-project-check.log`.
- These include the three Store contracts, three new Passport policy cases, eight
  native/browser recovery cases, all nine Chromium scenarios, TypeScript reference
  interoperability, namespace migration, carriers/lifecycle, structural bare-WASM,
  dependency checks, and Healthy/tooling compatibility.
- A private copy of the user's local Authy SQLite database started with the repaired
  executable. All credential, session and signing-key rows compared equal before
  and after registration. The probe removed its owned copy; the source was read-only.

During this checkpoint, repository guidance moved maintained architecture and Authy
compatibility into `ARCHITECTURE.md` and `apps/authy/CONTRACT.md`. The active review
has an explicit exception preserving this ledger and its inputs until it closes.
Concurrent documentation consolidation is outside the fixed product-code delta.
After closure, the remaining docs tree will be retired per AGENTS.md; the complete
review evidence will remain under `/tmp/opencode` and in local Git history.

Standards round 2 returned CLEAR for the validation HEAD. STD-1 and STD-2 are
resolved, with no repair-caused Standards blocker. Complete report:
`/tmp/opencode/module-pattern-standards-round2.md`. The reviewer independently ran
three Store cases, four policy/migration cases, and the shared native password/session
SDK contract; all passed.

Spec round 2 also returned CLEAR for the validation HEAD. SPEC-1, SPEC-2 and SPEC-3
are resolved; no repair-caused blocker or decision remains. Complete report:
`/tmp/opencode/module-pattern-spec-round2.md`. The reviewer independently ran three
Store cases, five wire/policy cases, the native password/session SDK journey and
eight native/browser recovery cases; all passed.

## Closure

Two rounds used, budget closed. Standards and Spec are CLEAR for
`72954a3dbd0e27b2400e186067f176c4f22d1c21`. Both required gates pass on that code.
Every accepted finding is resolved, and there are no independent follow-ups.
No remote, publication, new worktree, or tracker was introduced. A final ledger
copy and all four complete reviewer reports are retained under `/tmp/opencode`.
The historical documentation inputs remain recoverable in Git when the docs tree
is retired under the current repository guidance.
