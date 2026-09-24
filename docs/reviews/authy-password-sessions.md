# Authy password/session review

Status: **READY** on 2026-09-24. Reviewed product revision:
`f2f964777691872d7c52a8d5170b3d7779bed2a7`. Both review axes are CLEAR and the full
repository and Authy project checks pass. Two review rounds used; review is closed.

## Contract and revisions

- User authorization: first password/session flow with the framework and WebSocket
  work needed to sign in, implemented unattended for later human review.
- Contract: `docs/plans/authy-password-sessions.md`.
- Immutable base: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
- Worktree: `/home/cc444/code/snapco/snap`, existing `main` branch.
- No remote, tracker, or publication workflow exists. Keep records locally.
- Review budget: new Authy scope, independent of the closed tooling and private
  output reviews. Both authorized rounds are complete and recorded below.

Supported assumptions: Linux native host, browser WASM client, one host process
with a local SQLite database, same-origin HTTP/WebSocket browser traffic, explicit
public origin behind TLS proxies. Platform code owns concrete IO, time, randomness,
hashing, cookies, tasks, and storage. Portable application/runtime/client code is
`no_std` with `alloc`.

The selected Message operations are reads. Every physical reattachment creates a
fresh epoch and client reads resynchronize; detached mutation retention/replay is
outside this slice. Session and account persistence are intentional defaults.
Password reset, changes/assurance, passkeys, OAuth, documents, and distributed
session storage are excluded.

## Verification before review

- Native/browser password/session SDK contract: passed both adapters.
- Real Chromium packaged UI and development WebSocket proxy: passed.
- Independent Protocol contract: passed, including cookie flags, domain error
  envelopes, identity modes, admission fences, revocation and restart persistence.
- TypeScript reference Passport/Transport/BrowserTransport SDK against Rust: passed.
- TypeScript checking and workspace Clippy: passed.
- The first full gate found a cargo-machete false positive for Authy's generated
  wasm-bindgen async exports. Added the same documented exception used by Healthy.
  Portability and warnings-denied documentation had already passed in that run.
- The next full gate passed Rust/dependency/CLI/SDK/protocol/reference checks and
  eight Chromium cases. Healthy's Rust watcher case exposed another readiness vs
  acceptance race before its invalid-config edit. The test read the new Build
  while that candidate was still provisional, then superseded it itself. Added
  the existing `Rust generation ready: <Build>` acceptance wait at that transition.
  The assertion and product behavior remain unchanged. The failure trace is saved
  at `/tmp/opencode/authy-watcher-gate-failure.zip`.
- Its focused rerun also exposed an uncaught ECONNREFUSED while restoration was
  restarting Vite. Build polling now treats that expected outage as provisional
  evidence and still requires a successfully fetched matching Build to proceed.
- Full `mise exec -- ./bin/check` passed on
  `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`: Rust format/Clippy/portability/docs,
  dependency checks, CLI suites, Healthy SDK/protocol/journey/reference, Authy
  wire/reference, all nine Chromium scenarios, and listener replacement/cleanup.
  Log: `/tmp/opencode/authy-full-check.log`.
- `mise exec -- ./bin/snap check apps/authy` passed, including invocation of its
  configured checks from the application directory. Log:
  `/tmp/opencode/authy-project-check.log`.

The shared SDK journey exposed a real ordering defect during implementation:
release can terminate the caller's socket before its HTTP response arrives. The
controller now clears authenticated observations while preserving that pending
HTTP result. Native and browser adapters pass the same release-all regression.
HTTP/session and socket generations separately fence late results.

## Review ledger

### Round 1

Reviewed implementation: `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
Base: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
Commit: `Add Authy password sessions with persistent Passport and WebSocket clients`.

Standards and Spec run independently on that fixed diff. Model discovery confirmed
`openai/gpt-6-astra`; reviewers use the harness's Astra default without a model
override, as required by the tool instruction. The assignment requests particular
attention to authentication, persistence, and concurrent lifecycle ordering.

Supplied verification includes all focused Authy cases and the repaired Healthy
watcher case. The full repository gate is running on this committed code; final
readiness remains conditional on its result. No reviewer is asked to mutate code
or launch another review. Complete reports and session IDs will be retained here.

- Standards session: `ses_f2abdab73ffeVrcx4lMJT7Rp20`.
- Spec session: `ses_f2abd53aaffeoW1m8rclNTO4T9`.

Both round-1 reports are BLOCKED. Complete reports are preserved in
`authy-standards-round1.md` and `authy-spec-round1.md` beside this record.

| IDs | Disposition | Repair |
| --- | --- | --- |
| STD-1 / SPEC-1 | Accepted blocker | Preserve HTTP 409 Build mismatch and use browser reload/native close. |
| STD-2 / SPEC-2 | Accepted blocker | Separate untrusted/undecodable completion from a known remote failure; refetch after uncertain mutations without retry. |
| STD-3 / SPEC-3 | Accepted blocker | Drive portable close in the browser and retain its final immutable closed observation. |
| STD-4 | Accepted blocker | Give Pong writes the same bounded-write policy as other socket frames. |

The three overlapping findings were reproduced by both reviewers through public
SDKs. STD-4 is a direct missing-deadline path established by source inspection.
No scope decision or independent follow-up is needed. One repair batch and one
bounded validation round remain. The full and project checks passed before these
findings; the regression cases will extend their coverage.

### Repair batch

- STD-1 / SPEC-1: HTTP adapters retain response status, recognize 409 before reading
  its body, and send a distinct Build-mismatch input to the controller's existing
  reload/native-termination path. The Healthy query interface remains unchanged.
- STD-2 / SPEC-2: Completion decoding now separates a validated remote outcome from
  failure to obtain/decode/correlate one. The latter rejects the original command
  and refetches identity exactly once. Known domain failures keep their prior path.
- STD-3 / SPEC-3: Browser close drives `Input::Close`, publishes Rust's final state,
  settles pending commands, then drops owned HTTP/timer work. The facade retains
  and notifies the final immutable snapshot, shares its close Promise, and frees
  the binding after shutdown. Test adapters now separate SDK close from disposal.
- STD-4: All socket frame sends, including Pong and Close, use one timeout helper.
  Inspection confirms that `socket.send` occurs only inside that bounded helper.
  No TCP-buffer saturation timing claim is made.

Focused verification passed: eight new native/browser recovery cases, the shared
password/session journey through both SDKs, both real Authy UI scenarios, the wire
contract, TypeScript checking, workspace check and warnings-denied Clippy.
The new cases reproduce the reviewers' SDK-level triggers without importing the
controller, storage, or private host state.

### Round 2: fix validation

Repair revision: `f2f964777691872d7c52a8d5170b3d7779bed2a7`.
Prior reviewed revision: `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`.
The full `mise exec -- ./bin/check` passed again on the repair revision, including
all eight recovery cases, all nine Playwright scenarios, reference interoperability,
and the existing Healthy/tooling/structure/dependency gates. Log:
`/tmp/opencode/authy-fixed-full-check.log`.

Both original reviewers now validate their finding IDs and regressions caused by
the fixes, including affected interactions. This is the second and final review
round, not a fresh whole-diff review. No further autonomous repair/review round is
authorized if blockers remain. The application-directory project check is also
complete on the repair revision: `mise exec -- ./bin/snap check apps/authy` passed
with its added recovery command. Log: `/tmp/opencode/authy-fixed-project-check.log`.

Standards validation is **CLEAR**: STD-1 through STD-4 resolved, with no new
findings. Complete report: `authy-standards-round2.md`. The reviewer independently
ran the eight recovery cases and confirmed the sole socket-send timeout path.
Spec validation is **CLEAR**: SPEC-1 through SPEC-3 resolved, with no new findings.
Complete report: `authy-spec-round2.md`. The reviewer independently ran the eight
recovery cases and additionally checked close-Promise sharing, final notification,
snapshot immutability, and retained server-session authority after SDK close.

## Final disposition

| Findings | Final status |
| --- | --- |
| STD-1 / SPEC-1 | Resolved: HTTP Build lifecycle |
| STD-2 / SPEC-2 | Resolved: uncertain mutation recovery |
| STD-3 / SPEC-3 | Resolved: final browser closed observation |
| STD-4 | Resolved: bounded Pong writes |

No unresolved blockers, decisions, advisories, or independent follow-ups remain.
There is no tracker or publication step. Product and tests are committed locally;
the closure commit adds only documentation and complete review artifacts.

Implementation commits:

- `2f2f4cece05d61bfb0ad1853c27f6a3c46e888a2`: first password/session implementation.
- `f2f964777691872d7c52a8d5170b3d7779bed2a7`: reviewed lifecycle repairs and regressions.

The next feature should receive its own accepted scope. This completed review
budget must not be reused for another repair or quietly reset.
