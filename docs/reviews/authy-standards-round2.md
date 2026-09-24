# Authy password/session Standards review, round 2

Status: **CLEAR**. STD-1, STD-2, STD-3, and STD-4 are resolved. I found no evidenced regression caused by their fixes within this bounded validation scope.

## Revisions and scope

- Worktree: `/home/cc444/code/snapco/snap`.
- Previous reviewed commit: `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
- Fixed commit: `f2f964777691872d7c52a8d5170b3d7779bed2a7`.
- Inspected diff: `git diff 2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2...f2f964777691872d7c52a8d5170b3d7779bed2a7`.
- Original implementation base, for provenance: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
- HEAD matched the fixed commit. The only tracked working-tree modification at inspection was the coordinator's `docs/reviews/authy-password-sessions.md` update.

This was the second and final bounded review round. I inspected the four accepted repairs, their affected controller/platform/binding interactions, new consumer regressions and adapters, and check wiring. I did not reopen the original implementation as a new whole-diff review. The accepted password/session contract and scope exclusions remain unchanged. The plan's added text documents verification cases rather than changing requirements.

## Prior findings

| Finding | Prior disposition/severity | Validation result |
| --- | --- | --- |
| STD-1, duplicate SPEC-1 | BLOCKER, medium P2 | Resolved |
| STD-2, duplicate SPEC-2 | BLOCKER, medium P2 | Resolved |
| STD-3, duplicate SPEC-3 | BLOCKER, medium P2 | Resolved |
| STD-4 | BLOCKER, medium P2 | Resolved |

### STD-1: HTTP Build-change lifecycle is restored

Contract: `docs/plans/authy-password-sessions.md:101-102` requires browser reload and native client runtime termination on Build mismatch.

Both HTTP platforms now expose an internal status-bearing `exchange`. HTTP 409 returns before reading the response body. The identity adapters turn that result into `Input::BuildMismatch` instead of attempting to decode a completion. The controller checks the HTTP generation, resets session state and pending commands, and emits the existing reload action. The browser reloads the page; the native owner queues its close transition and cancels its jobs.

Inspected locations:

- `platforms/browser/src/lib.rs`, `Http::exchange`.
- `platforms/native/src/client.rs`, `Http::exchange`.
- `platforms/browser/src/identity.rs:130-140`.
- `platforms/native/src/client/identity.rs:127-138`.
- `crates/client/src/identity.rs:333-337`.

The focused regression run passed stale initial HTTP Build and anonymous Submit Build mismatch cases through both SDKs. The tests observe browser navigation or native closed state and verify a single request rather than mutation replay. Socket readiness is deliberately unavailable in this fixture, so it cannot mask a missing HTTP recovery path.

Result: **resolved**. No remaining blocker under STD-1.

### STD-2: Untrusted mutation outcomes cause one recovery fetch without replay

Contract: `docs/plans/authy-password-sessions.md:99` requires a refetch, never a resend, when HTTP failure leaves the mutation outcome unknown.

`decode_completion` now returns a nested result. Its outer failure means no trusted correlated outcome was obtained; its inner failure represents a decoded remote error. The HTTP command path treats outer failures as uncertain, rejects the original command, resets generations and observations, and schedules one `identity.fetch`. Known remote domain failures retain their previous behavior. The public completion helper still flattens the result for read-only socket handling.

Inspected locations: `crates/client/src/identity.rs:278-300,466-482` and its existing reset, fetch, and session-end paths.

Both SDK regressions passed malformed JSON and uncorrelated completion cases after a real account creation. The controlled carrier preserves the original Set-Cookie header. Assertions observe identified recovery, one account-creation request, and two total identity fetches, including startup. They also verify that subsequent close clears authenticated observations. The full supplied SDK journey separately retains wrong-password and duplicate-enrollment behavior.

Result: **resolved**. No remaining blocker under STD-2.

### STD-3: Browser close exposes the final portable closed observation

Contracts: `docs/plans/authy-password-sessions.md:88-91` assigns public lifecycle observations to Rust; lines 101-104 require explicit-close cancellation, pending-command rejection, and immutable stable binding observations.

Browser close now queues `Input::Close`. The owner publishes the portable closed snapshot, processes disconnect and command-completion actions, and returns. Dropping its owned futures cancels HTTP/timer work; socket teardown removes callbacks and closes the physical socket. The completion signal follows owner termination.

The facade rejects new commands as soon as close starts. It retains one closing Promise, awaits owner shutdown, decodes the final Rust snapshot, notifies subscribers, then clears subscriptions and frees the binding. The final snapshot remains available through `getSnapshot()` with the existing freezing rules.

Inspected locations:

- `platforms/browser/src/identity.rs:73-78,113-118,260-270` and socket/future ownership.
- `apps/authy/client.ts:17-30,48-61`.
- `crates/client/src/identity.rs:198-218,457-460`.
- Native/browser test adapters and the native line-protocol bridge.

The new tests separate SDK close from fixture disposal, so closed-state assertions inspect the retained public SDK rather than a destroyed page or process. My focused run passed close during a held command for native and browser: close completed, the command rejected, and the snapshot reported `phase: "closed", pending: false, identityId: null`. The shared journey now also asserts closed phase and cleared pending state; its supplied full-gate run passed through both clients.

Result: **resolved**. No remaining blocker under STD-3.

### STD-4: Pong writes use the bounded frame writer

Contracts: `docs/plans/authy-password-sessions.md:41-43` requires write deadlines; lines 34-36 require live-socket expiry and termination after local revocation.

The Ping branch now sends Pong through the common `write` helper with a five-second timeout and exits the connection loop on failure. Text frames use the same helper with five seconds. Close frames retain their one-second timeout. The helper wraps the actual send future in `tokio::time::timeout`.

Inspected locations: `platforms/native/src/websocket.rs:106,126-157`.

A source search found exactly one `socket.send`, at line 137 inside the timeout helper. A backpressured Pong therefore no longer holds the connection owner and its permit indefinitely. The expired write causes loop exit and ordinary connection cleanup.

Result: **resolved by source validation**. No TCP-buffer saturation measurement was run or inferred. This matches the evidence basis of the original finding.

## Affected interactions and regression assessment

- The new Build-mismatch input uses the HTTP generation. Socket generations and the existing release-versus-4001 ordering remain separate. Reset rejects pending commands and fences late results before reload or recovery.
- The uncertain-result change schedules recovery only from the command path. A failed recovery fetch follows the existing error-state path rather than replaying a command or entering an automatic refetch loop.
- The decoded remote-error path preserves known domain refusals. The socket completion interface keeps its prior outward result shape.
- The browser close transition settles pending commands before owned work disappears. The facade retains the final observation after freeing the binding and shares its closing Promise.
- Healthy's public query method signatures remain intact. The new internal exchange behavior is used by the identity lifecycle, and the supplied Healthy consumer checks passed.
- The fixes retain the portability boundary. The new controller input and result distinction are IO-free; status inspection, reload, tasks, and timeouts remain platform responsibilities. No package or dependency was added in this repair batch.
- New tests use public SDKs and a controlled HTTP carrier backed by real Authy authority. They do not call the controller or inspect storage/queue internals. Fixture disposal remains adapter-owned. Both repository and Authy project checks include the new recovery suite.

No new BLOCKER, FOLLOW_UP, ADVISORY, or DECISION finding was established from these fixes. There are no partial or unresolved prior findings.

## Checks and evidence

Personally performed:

1. HEAD/working-tree inspection and the fixed OLD...NEW diff review.
2. `git diff --check OLD...NEW`, passed.
3. Source validation of all explicit server socket sends and their timeout callers.
4. `bun test tests/sdk/identity-recovery.test.ts`, passed: **8 tests, 0 failures, 44 expect calls**, in 1.91 seconds. Both native and browser cases ran against owned ephemeral resources using existing artifacts. No project build was launched.

Focused test log: `/tmp/opencode/authy-standards-round2-tests.log`.

Supplied verification, not independently rerun as a full gate:

- Cargo check, warnings-denied Clippy, and TypeScript checking.
- Shared native/browser identity journey, Authy wire contract, packaged/dev UI, and reference TypeScript Passport/Transport/BrowserTransport interoperability.
- `mise exec -- ./bin/check` passed on `f2f964777691872d7c52a8d5170b3d7779bed2a7`, including Healthy SDK/protocol/journey, CLI, bare-WASM structure, Rustdoc, dependency gates, eight recovery tests, and all nine Playwright scenarios.

I inspected the tail of `/tmp/opencode/authy-fixed-full-check.log`; it confirms the recovery results, reference interoperability, all nine Playwright passes, and the final listener replacement/cleanup pass. Full-gate completion and revision attribution are also recorded in the supplied coordinator evidence and ledger. The separate application-directory project-check rerun remains the coordinator's responsibility; I did not compete for its build output or claim its result.

## Limits and final disposition

This is a completed bounded fix validation, not a new general authentication or security audit. It does not extend the accepted single-host password/session scope. STD-4 was validated from source rather than a saturation experiment. Final-snapshot immutability, Promise sharing, and generation fencing were checked in source; the focused tests independently exercise their relevant observable lifecycle outcomes.

No product files were edited, no commits or publications were made, and no review was delegated. The report and focused-test log are the only artifacts I created for this validation, under `/tmp/opencode`.

Final Standards result: **CLEAR** for `f2f964777691872d7c52a8d5170b3d7779bed2a7`. All four accepted Standards blockers are resolved. This consumes the second and final review round; no further autonomous review round is proposed.
