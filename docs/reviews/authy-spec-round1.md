# Authy password/session Spec review, round 1

## Result

**BLOCKED.** Three introduced violations of the accepted client lifecycle contract are confirmed. Two affect recovery through both SDK implementations. The third affects the browser's public closed-state observation. No product files were changed.

| ID | Disposition | Severity | Finding |
| --- | --- | --- | --- |
| SPEC-1 | BLOCKER | Medium, P2 | HTTP Build mismatch bypasses browser reload and native runtime termination |
| SPEC-2 | BLOCKER | Medium, P2 | An undecodable mutation response leaves a committed session unreconciled |
| SPEC-3 | BLOCKER | Low, P3 | Browser close never publishes the promised closed state |

No external decision is required to resolve these findings. The full repository gate remains for the coordinator to collect. Passing that gate would not resolve these independently reproduced cases.

## Reviewed revisions and scope

- Worktree: `/home/cc444/code/snapco/snap`.
- Immutable base: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
- Implementation HEAD: `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
- Diff: `git diff 11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8...2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
- Commit: `Add Authy password sessions with persistent Passport and WebSocket clients`.
- Accepted reference revision: `9689a8ed3108f58233721c2000d2b9ea96259fe7` in `/home/cc444/code/bod/snap`.

I read `AGENTS.md`, `README.md`, `TESTING.md`, the accepted plan, and the review record. The review record already had coordinator-owned uncommitted changes when inspection began. Product source matched the named implementation HEAD.

The reference checkout is currently at `188d32bfe720a85e336f31dded36bcd8f06f1069`. I read the selected revision's identity operation schemas and Passport session/server/client behavior with `git show`. A revision comparison found no changes between the selected revision and reference HEAD in `apps/authy`, `packages/core/src/identity`, `packages/engine/src/passport`, `packages/engine/src/transport`, or `packages/browser/src/transport*`. I did not silently substitute current reference behavior for the selected contract.

Scope is the authorized password/session flow, local SQLite persistence, signed 30-day cookies, authenticated read-only credential/session Messages, physical reconnect with a fresh epoch, and cross-client revocation. I did not require detached mutation replay, full identity/assurance, reset, OAuth, passkeys, distributed storage, or new retention guarantees.

## Findings

### SPEC-1: HTTP Build mismatch bypasses the required lifecycle transition

- **Disposition:** BLOCKER.
- **Severity:** Medium, P2.
- **Location:** `platforms/native/src/client.rs:72-80`; `platforms/browser/src/lib.rs:91-100`; `crates/client/src/identity.rs:275-319`. Compare the only Build-mismatch transition at `crates/client/src/identity.rs:380-383`.
- **Exact contract:** The plan's selected compatibility section retains Build negotiation. Its client lifecycle section, lines 101-102, says that browser Build mismatch requests page reload and the native client terminates its owning runtime.
- **Trigger:** A client issues its initial `identity.fetch` with a stale Build, or an anonymous page remains open across a server Build change and submits a command. An anonymous client has no WebSocket through which to receive close code 4003.
- **Evidence:** The native host correctly returns HTTP 409 with `{"error":"Snap Build mismatch"}` at `platforms/native/src/lib.rs:308-315`. Both HTTP adapters discard response status and return only text. The controller feeds that body into completion decoding and records `ContractViolationError: Mismatched completion`. Only a socket disconnection with code 4003 emits `Action::Reload`.

  I reproduced the initial-fetch case through the browser facade and the existing native SDK bridge against a real Authy host. The browser stayed on the same page in `phase: "error"`. The native client stayed live in the same error state and accepted `refresh`, which repeated the stale request. Probe output is at `/tmp/opencode/authy-spec-probe.log:1-18`.
- **Impact:** The documented upgrade recovery does not work for HTTP-only or pre-socket identity flows. Browser Retry cannot repair the stale Build, and native consumers do not receive the promised runtime termination.
- **Bounded remedy:** Preserve HTTP Build-mismatch information through the platform/controller boundary and route it through the existing reload/close lifecycle transition. Keep mutation retries disabled. Verify the stale initial-fetch and anonymous-command paths through the SDK adapters, in addition to the existing raw WebSocket 4003 check.

### SPEC-2: An undecodable mutation response leaves a committed session unreconciled

- **Disposition:** BLOCKER.
- **Severity:** Medium, P2.
- **Location:** `crates/client/src/identity.rs:275-289`, with the anonymous refresh behavior at `238-248` and completion parsing at `450-459`.
- **Exact contract:** The plan's client lifecycle section, lines 98-99, requires resolving current identity after a command whose outcome is unknown, without resending the mutation. The implementation comment at lines 280-281 makes the same promise for a lost response.
- **Trigger:** `account.create` commits and the carrier stores its Set-Cookie header, but the response body is incomplete or otherwise cannot supply a valid correlated completion. This also applies to a gateway replacing a mutation response body.
- **Evidence:** The controller first converts body-decoding failures into `ContractViolationError`. It treats only `UnavailableError` as an uncertain mutation outcome. Therefore it rejects the command but does not reset/refetch identity. An anonymous client's public `refresh()` is also a no-op unless its phase is `error`, so it cannot repair this state.

  I reproduced this through both real public SDK implementations using an isolated HTTP proxy. The real Rust host returned successful account creation with `sessionChanged: true`; the proxy retained the actual Set-Cookie header and replaced only the body with `{`. Both SDKs rejected the command as `Invalid completion`, stayed anonymous, and sent zero additional `identity.fetch` requests even after explicit refresh. A subsequent password sign-in failed with the real server's `IdentityForbiddenError`, proving that the stored cookie already identified the supposedly anonymous client. See `/tmp/opencode/authy-spec-probe.log:29-61`.
- **Impact:** A successful account creation can strand the usable sign-in flow with a valid server session and anonymous client state. The client cannot distinguish this from an operation that never committed, but handles it as a settled failure.
- **Bounded remedy:** Distinguish a validated server failure completion from a local failure to obtain/decode/correlate a completion. The latter must reject the original command once and refetch current identity under the existing generation fencing. It must not resend the mutation. Add a shared SDK recovery case that loses or replaces a mutation body after its cookie reaches the carrier.

### SPEC-3: Browser close never publishes the promised closed state

- **Disposition:** BLOCKER.
- **Severity:** Low, P3.
- **Location:** `platforms/browser/src/identity.rs:73-78`; `apps/authy/client.ts:28-31,44-50`. The portable close transition is at `crates/client/src/identity.rs:441-445`.
- **Exact contract:** The plan's client lifecycle section, lines 88-89, promises public observations that distinguish loading, anonymous, identified, error, and closed. The browser `Snapshot` type also exposes `phase: "closed"`.
- **Trigger:** Call and await the browser facade's public `close()`, then read `getSnapshot()`.
- **Evidence:** Browser close aborts the driver directly, so the portable controller never receives `Input::Close`. The facade sets `closed = true` and disables callbacks before awaiting that abort, then retains its old snapshot. Thus neither the platform snapshot nor the facade can enter the declared closed state. Native close does drive `Input::Close` and publishes it.

  The browser probe started an anonymous client, awaited close, and read `phase: "anonymous"`, rather than `closed`. See `/tmp/opencode/authy-spec-probe.log:20-28`. The same unchanged-snapshot path would retain a previously identified snapshot and its collections.
- **Impact:** The two SDKs disagree on a documented lifecycle observation. Browser consumers retain a live-looking snapshot after shutdown.
- **Bounded remedy:** Drive and publish the portable close transition before completing browser shutdown, while still cancelling owned IO/timers and rejecting pending commands. Let the facade retain that final immutable closed snapshot before suppressing further notifications. Keep the persisted server session intact. Verify final phase, cleared authenticated observations, and rejection of commands through the public browser SDK.

## Coverage

| Area | Inspection and evidence | Assessment |
| --- | --- | --- |
| Account creation | Portable validation, normalized credential claim, Argon2id hashing, SQLite enrollment transaction, cookie projection, SDK and wire cases | Basic flow is implemented; response-recovery violation is SPEC-2 |
| Password sign-in and sign-out | Identity modes, credential lookup/verification, post-verification credential/hash fence, current/others/all/session release paths, void and Approved envelopes | No additional confirmed blocker in inspected successful paths |
| Persistence and session authority | SQLite schema and transactions, persistent signing key and override, digest-only session rows, uniqueness, expiry filtering, cookie signature verification, duplicate cookie handling, restart wire/UI scenarios | Consistent with documented single-host defaults on inspection and supplied checks |
| Authenticated WebSocket reads | Canonical Origin, cookie resolution, authentication before epoch, operation lane and sequence admission, per-read authority resolution and transactional recheck, summaries, ack/completion | Selected read-only fresh-epoch design is implemented |
| Revocation and concurrency | Transactional caller recheck and ownership filter, session-ID broadcast, subscription before socket authentication, expiry timer, release socket-close-before-HTTP-response handling, independent HTTP/socket generations | The known release ordering fix is present; supplied shared SDK revocation checks support it |
| Admission and cancellation | Delivery correlation, retained operation permits during asynchronous work, accepted writes after observer loss, bounded connections/messages, platform-owned tasks | Inspected ownership agrees with the chosen flow; no failure-injection proof of every resource bound was attempted |
| Client recovery | Read deadlines, physical reconnect/backoff, fresh-epoch collection reads, pending HTTP preservation on socket loss, late-result fences | SPEC-1 and SPEC-2 expose uncovered recovery paths |
| Public observations and close | Rust snapshot ownership, immutable facade values, subscriptions, platform cancellation, native closed transition | Browser final state violates the contract, SPEC-3 |
| Application composition and portability | Authy application/native/WASM packages and role metadata, shared runtime/client modules, platform crypto/storage/IO ownership | No additional Spec blocker; supplied bare-WASM/workspace checks are relevant evidence |
| Development and packaging | Authy config, launch origin, selective development WS tunnel, frontend, packaged and dev browser scenarios | Usable slice is covered by supplied checks; no unrequested feature expansion identified |
| Healthy and existing behavior | Shared HTTP/dispatch/client diffs, Build exception, CLI origin addition, watcher assertion changes | No confirmed regression found; watcher changes retain the original assertions and add acceptance/transient-outage waiting |
| TypeScript compatibility | Selected operation schemas and Passport lifecycle source, Rust envelope/carrier behavior, reference script and supplied interoperability result | Success-path subset is supported. This review does not claim complete TypeScript Protocol compatibility |

## Checks performed and limits

I independently performed source/diff inspection and one focused executable probe. The probe used the existing Authy executable, existing native SDK bridge, generated browser bindings, the public TypeScript facade, Chromium, a temporary SQLite database, and owned ephemeral HTTP listeners. It performed no Cargo build, project build, or full suite. Its processes and temporary database were cleaned up.

Artifacts:

- Probe: `/tmp/opencode/authy-spec-probe.ts`.
- Complete output: `/tmp/opencode/authy-spec-probe.log`.
- This report: `/tmp/opencode/authy-spec-round1.md`.

The executable findings agree with the reviewed source. Artifacts were already built by the coordinator; I did not independently rebuild them. The short waits in the native Build probe collect output, rather than prove an eventual timeout promise. Source inspection establishes that no HTTP Build-mismatch transition exists, and the explicit refresh response establishes that the native runtime remains active.

I treated the supplied workspace check/Clippy, TypeScript, bare-WASM structure, rustdoc, dependency, CLI, Healthy SDK/protocol/reference, Authy SDK, packaged/dev UI, Authy wire, and TypeScript Passport/Transport interoperability passes as supplied evidence, not fresh results from this review. I did not read a completed result from `/tmp/opencode/authy-full-check.log`; the coordinator owns that running gate.

I did not independently simulate 30 days of clock progression, crash/power-loss storage failures, database contention, every concurrent request schedule, or TLS termination. Those limits do not require new features or expand this review's threat model. No authorization bypass or persistence-loss defect was confirmed in the inspected supported flow.

This is the single Spec inspection/report for round 1. No review was delegated, and no product edit, commit, publication, or issue creation was performed.
