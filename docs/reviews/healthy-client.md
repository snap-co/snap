# Healthy client implementation review

## Contract and revisions

The accepted milestone adds a native headless Healthy journey, SDK edge contracts,
and a real React application over Rust bindings. The same IO-free Rust client
application owns polling, status, and sample history in both runtimes. Native
entrypoints own startup; the reusable browser host loads one Healthy application
definition, with no application-owned init file. Platform adapters own IO. A
release executable serves HTML, JavaScript, and WASM from its adjacent web directory.

Healthy is an anonymous health check only. Auth, Snapshot, Hooky, arbitrary queries,
native UI bindings, simulation execution, hot reload, and deployment-provider
provisioning are excluded. The client application seam is explicitly single-flight
for this slice. Inputs include potentially malformed/unavailable HTTP responses;
SDK callers and the selected build/application files are trusted local consumers.

This repository had no commits. The root commit records the empty repository;
the implementation commit includes the earlier uncommitted spike as well as this
milestone. No pre-existing files were discarded.

- Base: `a0c98cbdc37c512c234a064623147cf769a50e2b`
- Implementation: `acd20733ec5a9e9d49b393ba09b588bceef75d39`
- Diff: `git diff a0c98cbdc37c512c234a064623147cf769a50e2b...acd20733ec5a9e9d49b393ba09b588bceef75d39`
- Commit: `Build Healthy as a headless and browser Rust client application`

## Verification before review

`./bin/check` passed on the implementation: Rust formatting and strict Clippy,
bare-WASM compilation, native example builds, four native SDK contracts, release
and WASM builds, TypeScript checking, three browser-runtime SDK contracts, shared
health contract through reference TypeScript/native Rust/WASM, protocol contracts,
native journey, Chromium failure/recovery rendering check, and dev runner lifecycle.

Chromium installation initially hit the system temporary-directory quota. Retrying
with the repository's ignored `.tmp` succeeded. Runtime/build checks have no open
environment blockers. The reference TypeScript compatibility test uses the installed
`~/code/bod/snap`; ordinary application builds do not depend on that checkout.

## Review budget

Round 1 is recorded before dispatch: independent Standards and Spec reviews of the
same immutable implementation. Reviewers are read-only; the coordinator owns fixes.
Astra is available. The harness forbids specifying a model without an explicit
user selection, so reviewers use the configured default instead of a forced variant.
At most one subsequent fix-validation round is permitted.

No issue tracker or publication workflow is configured in this repository.

## Standards report

Round 1 session: `ses_f2f956dd1ffeuPO790z8hOk4TA`.

### Complete report

**Axis: BLOCKED.** One confirmed wire-compatibility violation. The reviewed
application/core boundaries and consumer test seams otherwise follow the documented standards.

Revisions and coverage:

- Base: `a0c98cbdc37c512c234a064623147cf769a50e2b`
- HEAD: `acd20733ec5a9e9d49b393ba09b588bceef75d39`
- Reviewed `BASE...HEAD`, with emphasis on native/browser client runtimes, the
  synchronous application seam, Healthy state ownership, WASM/TypeScript bindings,
  React hosting, test organization, and release packaging.
- Read `AGENTS.md`, `README.md`, `TESTING.md`, and this review record.
- Inspected the reference implementation at its documented revision,
  `9689a8ed3108f58233721c2000d2b9ea96259fe7`.

**STD-1 · BLOCKER · Health Query accepts HEAD, contrary to the reference wire behavior**

Location: `platforms/native/src/lib.rs:102–108`, particularly `Lane::Query => get(query)`.

Requirement: `AGENTS.md:14–15` requires preservation of the selected TypeScript wire
behavior. `README.md:242–244` includes route/method rejection in the compatibility slice.

Trigger and evidence: Send `HEAD /health/up` with the server's matching
`x-snap-build` header. Axum's `get(query)` also handles HEAD. The native host therefore
executes the health query and returns success. A read-only HTTP probe against the
built native host returned GET 200 and HEAD 200. The equivalent probe against the
reference Bun HTTP host, using its real health route metadata and matching Build,
returned GET 200 and HEAD 405. The reference method restriction is explicit in
`~/code/bod/snap/packages/bun/src/http.ts:1195–1199`. The Rust route's fallback does
not catch HEAD because Axum treats it as handled by `get`.

Bounded remedy: Explicitly reject HEAD on the Query route. Add the matching-Build
HEAD case to `tests/protocol/healthy.contract.ts`, which currently checks POST
rejection at line 41. Keep the assertion at the HTTP Protocol interface.

Standards assessment:

- Application, shared runtime, protocol, and client core remain `no_std` with
  `alloc`. Their dependencies respect the portability boundary.
- Healthy owns polling policy, status, and bounded history in synchronous Rust
  processing. Platform runtimes execute HTTP, deadlines, waits, cancellation, and timestamps.
- React renders SDK observations. Application selection runs through the reusable
  browser entrypoint.
- Native journeys consume the SDK directly; runners own construction, deadlines,
  and cleanup. Edge, protocol, and browser checks are separately organized. No
  behavior tests call `Module::update` or inspect private application state.
- Packaging produces the executable and adjacent HTML/JS/WASM assets, with asset
  discovery owned by the native host.

Actually run: `git diff --check BASE...HEAD` passed; bare-WASM checks for healthy
and snap-client passed; targeted native and reference HTTP method probes on
ephemeral ports confirmed STD-1 and cleaned up; revision/working-tree inspection.
The full `./bin/check` pass was supplied evidence, not rerun by this reviewer.
Because the base is empty, history cannot distinguish earlier spike code from
milestone additions. STD-1 is a confirmed violation in the reviewed revision,
not a claim about when it was first introduced. No product edits, commits,
publication, or further delegation occurred.

## Spec report

Round 1 session: `ses_f2f9520ddffeMqbVwjABodLy3Y`.

### Complete report

**Spec axis: BLOCKED.** One confirmed compatibility violation: the Rust host
accepts `HEAD /health/up`, while the pinned TypeScript host rejects it. The reviewed
client ownership, lifecycle, and packaging paths otherwise meet the accepted milestone.

Revisions and scope:

- Base: `a0c98cbdc37c512c234a064623147cf769a50e2b`
- Implementation: `acd20733ec5a9e9d49b393ba09b588bceef75d39`
- Comparison: `git diff BASE...HEAD`
- Reference verified at `9689a8ed3108f58233721c2000d2b9ea96259fe7`.

Read AGENTS, README, TESTING, and this review record. Reviewed both client runtimes,
the portable application and Healthy state machine, WASM bindings, TypeScript
facade, React host and renderer, journey runner, test adapters, release scripts,
and relevant HTTP host/reference behavior. The empty base includes the earlier
spike; this finding identifies a current accepted-contract violation, not when
that host behavior was introduced.

**SPEC-1 · BLOCKER · Health route accepts a method rejected by the reference**

Locations: `platforms/native/src/lib.rs:104–108` and `:191–198`; missing protocol
assertion beside `tests/protocol/healthy.contract.ts:41`.

Trigger: Send `HEAD /health/up` with the server's matching `x-snap-build` value.
Axum's `get(query)` also routes HEAD to the handler, and `query` does not reject it.
The health invocation succeeds and returns HTTP 200 with an empty body. Reproduced
against `dist/healthy` with matching Build, status 200, body empty.

An ephemeral host using the pinned reference's real `BunHttp` implementation and
declared `Health.up` operation returned 405 for the same matching-Build request.
`~/code/bod/snap/packages/bun/src/http.ts:1195–1200` requires GET for Query operations.
The accepted milestone preserves TypeScript health wire behavior, and README
explicitly includes route/method rejection. Bounded remedy: explicitly reject HEAD
and add a matching-Build HTTP Protocol assertion expecting 405.

Coverage conclusions:

- Native headless journey CLEAR: SDK observations; runner owns construction,
  deadline, cleanup, and unsuccessful exit.
- Shared Rust application CLEAR: IO-free Healthy owns polling, status, supplied
  timestamps, and the 60-sample window.
- Platform ownership CLEAR: native and browser Rust adapters execute HTTP,
  deadlines, timers, and cancellation.
- Bindings/React CLEAR: TypeScript adapts immutable observations; React renders
  snapshots; shared entrypoint loads the definition without per-app init.
- Lifecycle CLEAR on reviewed paths and targeted probes: resident close aborts
  outstanding browser work; task ownership and drop/close release runtime work.
- Packaging CLEAR: copied executable-plus-assets directory runs outside checkout.
- Wire compatibility BLOCKED by SPEC-1.
- Scope discipline CLEAR: no broader framework or excluded features required.

No separate FOLLOW_UP, ADVISORY, or DECISION findings.

Checks actually run:

- Verified implementation/reference revisions and tracked implementation files.
- Probed the public WASM/TypeScript SDK over real HTTP: closing a resident client
  during a pending request triggered abort and settled close; a hanging query
  failed with UnavailableError at approximately 5,001 ms and triggered cancellation;
  the same query client recovered to return `{ status: "OK" }`.
- Copied dist into a temporary directory under `/tmp/opencode`, launched from an
  unrelated working directory without SNAP_WEB_DIR, and verified HTML, JS, CSS,
  WASM, and successful health responses.
- Compared method handling on real Rust/reference hosts and confirmed SPEC-1
  against the release artifact.

The complete coordinator `./bin/check` pass was supplied evidence, not rerun.
Additional WASM probes ran under Bun, not Chromium; supplied Chromium evidence
covers packaged rendering/recovery. The 60-sample eviction logic was inspected
without a full 61-poll run. No product edits, commits, or publication occurred.
Temporary release-copy artifacts were cleaned up.

## Finding ledger and readiness

STD-1 and SPEC-1 are the same finding and are both accepted. Reference comparison
establishes the mismatch. Explicit HEAD
rejection and a Protocol regression assertion are in the repair batch. A rebuilt
native host passed the complete Protocol contract including HEAD rejection.
Post-repair `./bin/check` passed, including the new matching-Build HEAD assertion,
both native/debug and release builds, and Chromium against the release artifact.
`git diff --check` also passed. The user interrupted an attempted documentation
patch; it made no edits and did not cancel or restart either review round.

## Round 2: fix validation

Recorded before dispatch. Both existing reviewers will validate only STD-1/SPEC-1
and regressions from the small HEAD-route/test delta against their reviewed
`acd20733ec5a9e9d49b393ba09b588bceef75d39`. The complete round-1 reports and the
accepted duplicate finding remain above. This is the only fix-validation round.

Validated repair: `d1d6330cdf97ccc18319fb5a974e4965431bc2f7`.

### Standards validation report

Session: `ses_f2f956dd1ffeuPO790z8hOk4TA`.

**Axis: CLEAR. STD-1 is resolved.** No concrete regression was found in the bounded fix.

Reviewed previous `acd20733ec5a9e9d49b393ba09b588bceef75d39` against fixed
`d1d6330cdf97ccc18319fb5a974e4965431bc2f7`, including the exact diff and finding
ledger. Confirmed HEAD matches the fixed revision and the working tree was clean.

`platforms/native/src/lib.rs:105–106` now registers an explicit HEAD handler
returning 405 on Query routes. It overrides implicit HEAD-through-GET handling
and prevents HEAD from invoking the health query. The matching-Build regression
assertion at `tests/protocol/healthy.contract.ts:42–48` exercises the public HTTP
Protocol interface, consistent with the testing standards.

Actually run: repair `git diff --check` passed; the complete healthyProtocol
contract passed against an ephemeral native host, including HEAD rejection and
existing GET, invalid-input, POST, unknown-route, and Build assertions. Host cleaned
up. The complete post-repair bin/check pass was supplied evidence, not rerun.
Review was limited to STD-1 and fix regressions. No edits, commits, publication,
or delegation occurred. The sole fix-validation round is complete.

### Spec validation report

Session: `ses_f2f9520ddffeMqbVwjABodLy3Y`.

**Axis: CLEAR. SPEC-1 is resolved.** No concrete regressions were found in the bounded fix.

Reviewed previous `acd20733ec5a9e9d49b393ba09b588bceef75d39` against repair
`d1d6330cdf97ccc18319fb5a974e4965431bc2f7`. Confirmed HEAD matches the repair
and the working tree was clean. Scope was SPEC-1 and route/test-change regressions.

`platforms/native/src/lib.rs:104–107` explicitly returns 405 for HEAD on Query
routes, overriding automatic HEAD-to-GET handling and preventing health dispatch.
`tests/protocol/healthy.contract.ts:42–48` adds the matching-Build HTTP assertion.
This matches the reference behavior established in round 1. STD-1 is the same finding.

Actually run: inspected committed delta and ledger; complete Healthy Protocol
contract passed against both target/debug/examples/healthy and dist/healthy on
owned ephemeral hosts; independently confirmed matching-Build HEAD returns 405
with empty body on both artifacts, and subsequent GET returns 200 with correctly
correlated success. Both hosts were cleaned up. Post-repair bin/check and diff
checks were supplied evidence, not rerun in full. No remaining Spec blockers or
decisions. No edits, commits, publication, or delegation. The sole validation round
is complete.

## Final readiness

**READY.** Both axes are CLEAR on `d1d6330cdf97ccc18319fb5a974e4965431bc2f7`.
STD-1/SPEC-1 are resolved. Required checks passed for that code. No follow-up issues
or advisory findings remain. Two review rounds were used; no third review occurred.
The subsequent documentation commit only records these final reports.
