# Authy password/session review

## Contract and revisions

- User authorization: first password/session flow with the framework and WebSocket
  work needed to sign in, implemented unattended for later human review.
- Contract: `docs/plans/authy-password-sessions.md`.
- Immutable base: `11ce219cb5b3fc6c3ec2a65feddc45c9ded59ee8`.
- Worktree: `/home/cc444/code/snapco/snap`, existing `main` branch.
- No remote, tracker, or publication workflow exists. Keep records locally.
- Review budget: new Authy scope, independent of the closed tooling and private
  output reviews. Zero rounds dispatched at this checkpoint.

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
- Full gate completion remains pending at this checkpoint.

The shared SDK journey exposed a real ordering defect during implementation:
release can terminate the caller's socket before its HTTP response arrives. The
controller now clears authenticated observations while preserving that pending
HTTP result. Native and browser adapters pass the same release-all regression.
HTTP/session and socket generations separately fence late results.

## Review ledger

No findings yet. Both leaf reviews will receive the same committed revision and
contract. Reviewers inspect once without edits, commits, or further delegation.
