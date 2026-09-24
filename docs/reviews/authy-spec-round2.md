# Authy password/session Spec review, round 2

## Result

**CLEAR for this bounded fix validation.** SPEC-1, SPEC-2, and SPEC-3 are resolved. I found no evidenced regression caused by the repair batch in the affected interactions. No finding remains partial or unresolved, and no human decision is needed for the Spec axis.

| Prior ID | Prior disposition and severity | Round-2 status |
| --- | --- | --- |
| SPEC-1 | BLOCKER, Medium P2 | Resolved |
| SPEC-2 | BLOCKER, Medium P2 | Resolved |
| SPEC-3 | BLOCKER, Low P3 | Resolved |

This completes the second and final authorized review round. It is a validation of the accepted findings and their fixes, not a fresh whole-implementation review.

## Revisions and scope

- Worktree: `/home/cc444/code/snapco/snap`.
- Original immutable base: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
- Previously reviewed implementation: `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
- Fixed implementation: `f2f964777691872d7c52a8d5170b3d7779bed2a7`.
- Inspected repair diff: `git diff 2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2...f2f964777691872d7c52a8d5170b3d7779bed2a7`.
- Accepted compatibility reference remains `9689a8ed3108f58233721c2000d2b9ea96259fe7`.

HEAD matched the fixed implementation. The only worktree difference from it was the coordinator-owned `docs/reviews/authy-password-sessions.md`. I read that ledger, the repair diff, affected source and test adapters, the new recovery suite, and the Standards report's overlapping findings and Pong-write finding. The accepted behavior contract has no amendment; the plan changes add verification coverage.

The validation covers HTTP Build negotiation, uncertain mutation recovery, browser close and its binding lifetime, affected native lifecycle behavior, and the bounded server-write helper. Round-1 assessment of unrelated authentication, persistence, architecture, and excluded features is not reopened.

## Finding validation

### SPEC-1: HTTP Build mismatch lifecycle

**Resolved.**

Relevant fixed code:

- `platforms/browser/src/lib.rs`, `Http::exchange`.
- `platforms/native/src/client.rs`, `Http::exchange`.
- `platforms/browser/src/identity.rs:130-140` and `platforms/native/src/client/identity.rs`, HTTP action execution.
- `crates/client/src/identity.rs:333-338`, `Input::BuildMismatch`.

Both HTTP adapters now retain status and recognize 409 before reading its body. Identity drivers send a distinct generation-tagged `BuildMismatch` input rather than trying to decode the problem document as a completion. The controller checks the HTTP generation, clears prior session observations and pending commands, and emits the existing reload action. Browser execution requests page reload; native execution enters its existing close path and cancels owned jobs.

I independently ran the new native/browser SDK tests for both original triggers:

1. A stale initial `identity.fetch` before any socket attachment.
2. An anonymous Submit after the server Build changes.

All four passed. Browser assertions observe actual main-frame navigation. Native assertions observe `phase: "closed"` and rejection of further work. Request counts establish one initial fetch or one mutation attempt, with no mutation resend.

The shared HTTP change preserves the query method's public type. For the supported Healthy path, the host's readiness exception still avoids Build rejection. The supplied full gate also passes Healthy SDK/protocol/reference cases. I found no supported-flow regression from this change.

### SPEC-2: Unknown mutation outcomes

**Resolved.**

Relevant fixed code: `crates/client/src/identity.rs:278-300,466-482`.

Completion decoding now separates a validated remote outcome from failure to obtain a valid, correlated outcome. Invalid JSON, invalid envelopes, mismatched operation IDs, and undecodable remote errors take the untrusted-outcome path. An uncertain mutation rejects its original command and schedules one identity refetch through the existing reset and generation fence. It does not reissue the mutation.

A valid remote domain failure remains distinguishable and retains its previous handling. The public completion helper still exposes the same flattened outcome for socket reads, so this change does not redefine their application-visible errors. The existing session-ended flag still forces reconciliation when a revocation closes the socket before the HTTP command settles.

The independently rerun SDK tests passed for both native and browser clients, with two bodies per client: malformed JSON and a well-formed completion naming a different operation. Their controlled carrier preserves the real Rust host's original cookie headers after the account commits. The assertions observe an identified UUID, one account-creation request, and exactly two identity fetches including startup. The unavailable WebSocket in this fixture cannot supply a second recovery mechanism. These cases directly cover the round-1 defect without relying on private controller or database state.

### SPEC-3: Browser final closed observation

**Resolved.**

Relevant fixed code:

- `platforms/browser/src/identity.rs:73-78,113-118,260-270`.
- `apps/authy/client.ts:51-63`.
- `crates/client/src/identity.rs:198-218,457-460`.

Browser close now queues `Input::Close`, allowing the portable controller to clear identity, collections, pending state, and connection state and publish `phase: "closed"`. The driver settles pending command replies before returning and dropping its owned HTTP/timer futures. The completion signal follows driver termination. Socket callback detachment and physical close still belong to the existing socket owner.

The facade rejects new commands immediately, shares a single close Promise, reads the final Rust snapshot after shutdown, retains its immutable decoded value, and notifies subscribers before clearing them and freeing the binding. Fixture disposal is now separate from SDK close, so assertions can inspect the actual final observation rather than a destroyed page or process.

The independently rerun recovery suite passed pending-command close for both clients, including command rejection, bounded close completion, `phase: "closed"`, `pending: false`, and null identity. Its uncertain-mutation cases also close an identified client and verify cleared collections.

I additionally ran a focused browser-facade probe for the new final-notification and binding-lifetime interaction. It confirmed:

- Two close calls return the same Promise.
- A subscriber receives exactly one final `closed` notification.
- The retained snapshot and collection arrays are frozen, and repeated snapshot reads retain identity.
- Final connection is disconnected, identity is null, collections are empty, and pending is false.
- An independent HTTP identity fetch with the browser's cookie after SDK close still identifies the original account. Close did not sign out the persisted session.

## Other affected interactions

The STD-4 repair is consistent with the accepted write-deadline promise. The Pong path now uses the same bounded physical-write helper as other frames. Source search finds the server's only `socket.send` inside `tokio::time::timeout` at `platforms/native/src/websocket.rs:137`. Text/Pong writes retain five-second bounds; Close retains its one-second bound. Error or timeout leaves the connection loop. I found no new protocol behavior or lifecycle regression from this helper extraction. Standards owns the formal disposition of STD-4.

The new tests use public SDKs and a real Authy authority behind a controlled carrier. Their adapters own construction and cleanup. The native bridge's added final snapshot is read from the real SDK after close; it does not manufacture the asserted state. Gate changes add the recovery suite to both repository and application checks. No additional feature, persistence policy, or compatibility exception was introduced by the repair batch.

## Checks and evidence

Personally performed:

1. Fixed-revision and worktree checks, repair-diff inspection, and affected lifecycle/source inspection.
2. `bun test tests/sdk/identity-recovery.test.ts`: **8 passed, 0 failed, 44 assertions**, in 1.94 seconds. Log: `/tmp/opencode/authy-spec-round2-tests.log`.
3. Focused browser close probe: passed. Script: `/tmp/opencode/authy-spec-round2-close.ts`. Output: `/tmp/opencode/authy-spec-round2-close.log`.
4. Read the complete `/tmp/opencode/authy-fixed-full-check.log`.

The full-gate log supports the coordinator's reported successful `mise exec -- ./bin/check` on the fixed revision. It records the recovery suite, Authy wire contract, actual TypeScript SDK interoperability, both shared identity SDK scenarios, packaged/dev Authy UI scenarios, Healthy SDK/protocol/journey/reference checks, all nine Playwright scenarios, CLI suites, portability, documentation, dependency checks, and final listener-replacement cleanup. I did not rerun that gate or its builds.

The separate application-directory project check remains coordinator-owned. Its eventual result is not represented as an independent result of this validation.

## Limits and completion

Executable validation used the already-built fixed binaries and generated bindings, with owned ephemeral listeners, temporary databases, and Chromium contexts. I did not rebuild artifacts. The recovery peer deliberately refuses WebSocket attachment to isolate HTTP recovery. Normal WebSocket, revocation, and reconnect coverage comes from the inspected code and the supplied passing full gate.

I did not perform a TCP-buffer saturation measurement, exhaustive scheduler exploration, or a new whole-diff/security review. None is claimed by this result. No independent follow-up or new blocker was established within the authorized fix-validation scope.

No product files were edited. No commit, publication, issue creation, or delegated review was performed. This report and its probe artifacts are under `/tmp/opencode` only.
