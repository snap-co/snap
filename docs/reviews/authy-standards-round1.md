# Authy password/session Standards review, round 1

Status: **BLOCKED**. Four documented-contract violations need fixes. Three have direct browser SDK reproductions; the fourth is a bounded-write violation visible in the socket loop. No product files were changed.

## Scope and revisions

- Worktree: `/home/cc444/code/snapco/snap`.
- Immutable base: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
- Reviewed HEAD: `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
- Diff: `git diff BASE...HEAD`, the single commit `Add Authy password sessions with persistent Passport and WebSocket clients`.
- Read `AGENTS.md`, `README.md`, `TESTING.md`, `docs/plans/authy-password-sessions.md`, and `docs/reviews/authy-password-sessions.md`.
- The only worktree modification at inspection was the coordinator's review ledger. Product source matched the reviewed commit. The current ledger was read as review context, not included as an implementation change.
- Selected TypeScript reference: `9689a8ed3108f58233721c2000d2b9ea96259fe7`. The reference checkout currently reports `188d32bfe720a85e336f31dded36bcd8f06f1069`. I read the selected identity schema with `git show` and verified an empty selected-to-current diff for `packages/browser/src/transport.ts`, `packages/engine/src/passport`, `packages/core/src/identity`, and `apps/authy/account.ts`, which were the reference areas inspected.

The review accepts SQLite persistence, 30-day signed cookies, one local host, fresh epochs on physical attachment, and read-only Message operations. It does not require detached mutation replay, distributed authority, password reset, assurance, OAuth, or passkeys.

## Findings

### STD-1: HTTP Build mismatch never reaches the client Build-change lifecycle

- Disposition: **BLOCKER**.
- Severity: medium, P2.
- Location: `platforms/browser/src/lib.rs:91-100`; `platforms/native/src/client.rs:72-80`; `crates/client/src/identity.rs:275-319,380-383`.
- Applicable contract: `docs/plans/authy-password-sessions.md:101-102`: "Browser Build mismatch requests page reload. The native client terminates its owning client runtime." Lines 65-66 also retain Build negotiation. `AGENTS.md:20-21` requires preserving the selected wire behavior.
- Trigger and evidence: Start an Authy SDK with an outdated Build, including the anonymous client that has no socket yet. The real host returns HTTP 409 at `platforms/native/src/lib.rs:308-315`. Both HTTP adapters discard the status and return just the body. The controller treats the problem document as a malformed completion and enters `phase: "error"`. Only a WebSocket 4003 produces `Action::Reload`, and this client never reaches socket attachment. The browser probe produced `ContractViolationError: Mismatched completion`, with no reload. The native adapter feeds the same status-blind result into the same controller, so its close action is also unreachable on this path.
- Impact: An anonymous client surviving a server Build change cannot recover through the promised lifecycle. Retrying or signing in continues using the stale Build.
- Bounded remedy: Preserve the host's HTTP Build mismatch as a distinct adapter result/input and route it through the existing browser reload/native close action. Keep ordinary domain failures separate. Add a consumer-level stale-Build scenario before socket attachment, using the existing SDK adapters or their controlled peers.

### STD-2: An undecodable mutation response leaves session observations stale instead of refetching

- Disposition: **BLOCKER**.
- Severity: medium, P2.
- Location: `crates/client/src/identity.rs:275-290,450-459`.
- Applicable contract: `docs/plans/authy-password-sessions.md:99`: "HTTP failure with unknown mutation outcome causes a refetch, never a resend." The synchronous client owns late-result fencing and identity observations under lines 88-91.
- Trigger and evidence: A submitted account creation commits, but its HTTP response body is lost or replaced before the client can decode a correlated completion. The controller refetches only for success, `UnavailableError`, or a previously observed session-end close. Malformed JSON, a missing envelope, and a mismatched completion become `ContractViolationError`, although none establishes whether the mutation committed.

  The browser probe passed account creation to the real Rust host, preserved the response headers including Set-Cookie, and replaced only its body with invalid text. `createAccount` rejected with `Invalid completion`; the SDK stayed anonymous. A separate explicit wire fetch returned an identified UUID. The request counter recorded exactly that explicit fetch and no recovery fetch from the SDK.

- Impact: The cookie and server session can be identified while the resident SDK remains anonymous. The same controller behavior applies to native clients. This is also a realistic response-loss case for a proxy replacing an upstream response; the client lacks evidence of a rejected mutation.
- Bounded remedy: Distinguish failures before a trustworthy correlated outcome from valid operation errors. Reject the caller's uncertain command, clear/refetch session observations once, and never resend the mutation. Add this controlled-response failure to the shared SDK contract. Keep known domain failures, such as a wrong password, on their current path.

### STD-3: Browser close never publishes the promised closed state

- Disposition: **BLOCKER**.
- Severity: medium, P2.
- Location: `platforms/browser/src/identity.rs:73-78`; `apps/authy/client.ts:28-30,44-50`; compare `crates/client/src/identity.rs:441-445`.
- Applicable contract: `docs/plans/authy-password-sessions.md:88-89`: "Public observations distinguish loading, anonymous, identified, error, and closed state." Lines 101-104 assign explicit close cancellation and pending-command rejection to the SDK. Rust owns these observations under lines 88-91.
- Trigger and evidence: Await the public browser facade's `close()` and read `getSnapshot()`. Browser close aborts the owner without delivering `Input::Close`; the facade sets its own closed flag and suppresses notifications while retaining the old snapshot. Native close does drive `Input::Close`, so the two consumers disagree.

  The browser probe observed `phase: "anonymous"` both before and after a completed close. A second probe closed while account creation was pending. Close completed and the command correctly rejected with `Client is closed`, but the public snapshot still reported `phase: "anonymous", pending: true`.

- Impact: A successfully closed SDK continues reporting a live or pending client. This finding concerns the documented lifecycle state, not a new requirement to erase retained JavaScript values or revoke the persisted session.
- Bounded remedy: Let the browser owner process the portable close transition and expose its final immutable snapshot before completing binding cleanup. Make the facade retain that final snapshot. Verify the closed observation and pending-command rejection through the public SDK; adapter cleanup must not destroy the page before those observations can be inspected.

### STD-4: The server's Pong write bypasses the promised write deadline

- Disposition: **BLOCKER**.
- Severity: medium, P2.
- Location: `platforms/native/src/websocket.rs:104-106`, compared with deadline helpers at lines 126-148.
- Applicable contract: `docs/plans/authy-password-sessions.md:41-43`: the host bounds admitted work and live sockets, and "writes have deadlines." Lines 34-36 require live socket expiry and termination after local revocation. `AGENTS.md:17-19` requires lifecycle and cancellation promises to be enforced beside their interfaces.
- Trigger and evidence: The Ping branch directly awaits `socket.send(Message::Pong(bytes))` without a timeout. JSON events have a five-second write timeout and close frames have a one-second timeout, but this branch uses neither. If a connected peer stops draining server output while sending enough Ping traffic to backpressure the stream, the task can remain inside this write. The outer `tokio::select!` cannot poll expiry or revocation while that await is pending, and the task retains its live-socket permit.
- Evidence limit: This is source-established missing deadline coverage. I did not run a TCP-buffer saturation experiment or claim an observed duration on this machine.
- Bounded remedy: Apply the socket write deadline to Pong sends and leave the connection loop when it expires, releasing the permit. Keep all frame writes under the same bounded-write policy. A protocol-level slow-reader probe can verify this without testing private controller internals.

## Standards coverage

### Architecture and ownership

- Inspected the new Passport workflow, external-work/result types, host driver, identity controller, native/browser adapters, app composition packages, and binding facade.
- Portable application/runtime/client behavior remains synchronous and `no_std` with `alloc`; concrete database access, crypto, time, randomness, sockets, HTTP, and tasks remain in platforms or app-owned native composition.
- New packages establish Authy's portable application and its native/WASM compositions. Shared Passport and identity behavior use modules in existing crates. Shared platforms do not depend on Authy.
- New manifests declare Snap roles. Normal dependency direction matches the documented structure; the supplied bare-WASM structural gate is consistent with the inspected graph.
- New dependencies and lockfile changes support the selected native persistence, password, cookie, and socket implementation. Dependency verification is supplied evidence, not a separately repeated audit.

### Authentication and persistence

- Reviewed optional/forbidden/required identity dispatch, email normalization, JS UTF-16 enrollment password bounds, Approved/void/error output projection, duplicate claim enforcement, and account/session ownership checks.
- Argon2 password work runs outside the database mutex and IO task. Enrollment and its first session use one SQLite transaction. Session creation rechecks credential ID, identity, and hash after password verification.
- Protected collection reads and revocation recheck the session in their transaction. Revocation selects only the caller's identity. Resolve uses token digests and expiry checks.
- Cookie HMAC verification, active-name duplicate rejection, host-prefixed Secure cookies for HTTPS origins, HttpOnly/SameSite/Path/Max-Age, canonical origin handling, persistent key selection, and Unix database creation mode were inspected.
- Database transaction errors return failures; session authority is persisted before a cookie is projected. No automatic mutation replay was introduced.
- No additional authentication or persistence blocker was established within the agreed single-host password/session scope.

### Concurrent lifecycle and Transport

- Reviewed one-owner application execution, delivery correlation, work completion, retained admission permits after HTTP timeout, separate HTTP/socket generations, release response versus 4001 ordering, read coalescing, read deadlines, reconnect backoff, and owned task/socket cleanup.
- The existing release race fix preserves an in-flight HTTP command while clearing authenticated observations. The supplied shared SDK journey covers release-all through native and browser adapters.
- Revocation subscription is installed before socket authority resolution. Protected reads resolve authority again. Socket epochs are fresh per physical attachment; Message admission restricts operations to the read lane, fences sequence IDs, and does not introduce mutation replay.
- Reviewed Build rejection after WebSocket upgrade, connection capacity, input bounds, event/close writes, expiry, and revocation notifications. Findings STD-1 through STD-4 identify the remaining observed or source-established lifecycle gaps.
- Inspected Vite's path-specific upgrade tunnel and teardown. Its app socket handling remains separate from HMR, with raw headers and initial upgrade bytes forwarded.

### Consumer contracts and Healthy preservation

- New SDK assertions are shared through native/browser adapters. Protocol assertions own cookies, envelopes, identity modes, sequence admission, Build close behavior, and persistent authority. UI assertions cover packaged/dev flows and reload/reconnect/remote revocation.
- Tests use real public interfaces and owned temporary databases/listeners. I found no new behavior test coupled to `Module::update`, private queues, or SQL text.
- The Healthy watcher edits retain existing assertions while waiting for accepted generations and tolerating transient connection refusal during replacement. They do not weaken the accepted-Build requirement.
- Inspected the project/repository gate wiring, TypeScript inclusion, new adapter construction, and reference consumer. Full-suite preservation relies on the supplied results and the coordinator's running gate.

## Checks and evidence

Personally performed:

1. Fixed-revision diff and working-tree/HEAD checks.
2. Read-only source and reference inspection described above.
3. `git diff --check BASE...HEAD`, passed.
4. Focused browser SDK probes using the existing Authy native executable and generated WASM bindings. An owned host used an ephemeral port and a temporary SQLite database; Chromium contexts and the host were closed afterward. No Cargo/project builds or full suites were started.

Probe artifacts:

- `/tmp/opencode/authy-standards-probe.ts`
- `/tmp/opencode/authy-standards-probe.log`

The probe transpiles the existing facade/shared TypeScript in memory and serves it through Playwright routes on the owned host's origin. It exercises the actual browser Rust/WASM SDK. It does not replace the controller or add assertions against its private representation. The uncertain-mutation case modifies a carrier response after the real server commits. The pending-close case also confirmed that close completes and rejects the pending command; its failure is the retained observation.

Supplied, not independently rerun:

- Workspace check/Clippy, TypeScript, bare-WASM workspace structure, warnings-denied Rustdoc, dependency checks, and all CLI suites.
- Healthy SDK/protocol/reference checks.
- Authy native/browser SDK, packaged/dev UI, wire protocol, and actual reference TypeScript Passport/Transport/BrowserTransport interoperability.
- Focused Healthy watcher pass after the acceptance-wait and transient-refusal fixes.

The coordinator is collecting `/tmp/opencode/authy-full-check.log`. I did not treat the in-progress full gate as passed or compete for its shared build lock. A green full gate would not resolve the reproduced contract gaps above, which its present scenarios do not assert.

## Review limits and disposition

This completes the Standards axis's single round-1 inspection. It is not an independent replay of every supplied gate or a general security audit. Source inspection establishes STD-4; its backpressure trigger was not load-tested. No new threat model, retention guarantee, excluded feature, or distributed-storage obligation was used to classify a blocker. No additional reviewer was delegated, no issue was published, and no product edit or commit was made.

Overall result remains **BLOCKED** on STD-1, STD-2, STD-3, and STD-4. There is no separate human design decision requested by this report. Fix validation belongs to the remaining review round.
