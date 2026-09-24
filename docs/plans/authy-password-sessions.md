# Authy password sessions

Status: **READY**, 2026-09-24. Product revision
`f2f964777691872d7c52a8d5170b3d7779bed2a7` passes the full repository and Authy project
checks. Both final review axes are CLEAR; see the review record below.

## Accepted scope

On 2026-09-24 the user approved the first password/session flow and the Transport
upgrades it needs, with unattended implementation followed by human review. Work
uses the existing branch. The preceding tooling/ownership reviews are closed;
this implementation has its own two-round review budget.

Deliver account creation, password sign-in, sign-out, identity recovery after page
reload, authenticated credential/session reads over WebSocket, reconnect after a
temporary drop, and session revocation observable by another client. Browser and
native clients execute the same portable controller. Healthy's contracts remain.

Password changes, fresh assurance, password reset/email, passkeys, OAuth, document
replication, and distributed hosting belong to later flows.

## Implementation decisions

- Authy selects the `user` identity kind and `account.create` operation in its
  portable application package. The shared Passport module owns identification
  policy and password/session workflows. Platform code has no Authy dependency.
- Keep synchronous input/action execution. Passport requests typed external work;
  native workers return correlated completion inputs to the one application owner.
  The work interface is specific to this demonstrated flow, not a generic task DSL.
- Use SQLite for local account/session persistence. `SNAP_DATABASE` selects the
  database, default `.snap/authy.sqlite` under the working directory. Development
  runs in the app directory. Accounts, password hashes, sessions, and the local
  signing key survive native rebuilds/restarts. This is a single-host deployment.
- Passwords use Argon2id. Session tokens and salts use OS randomness. Only token
  digests enter the session table. The platform signs cookie tokens with HMAC-SHA256.
  A local signing key is generated once in SQLite, or `SNAP_SESSION_KEY` supplies
  an explicit key of at least 32 bytes. New database files have mode 0600 on Unix.
- Sessions last 30 days, matching the reference default. Every operation resolves
  session authority; protected reads and revocation recheck it in their transaction.
  Live sockets have an expiry timer and receive termination after local revocation.
- Credential claims and the first session commit together. Account creation with a
  duplicate normalized email fails. Session creation after password verification
  checks that the credential/hash still matches. Accepted writes can finish after
  an HTTP observer disappears; the client never retries mutations automatically.
- The HTTP host retains admission capacity until work completes, even if its reply
  observer times out. There are 64 admitted operations and at most 128 live sockets.
  HTTP bodies and socket messages are bounded to 64 KiB; writes have deadlines.
- `SNAP_ORIGIN` is the canonical public origin. Dev supplies its resolved public
  address unless overridden. A standalone server defaults to its listen address;
  port-zero launchers resolve that default after binding. TLS/proxy deployments
  supply the public HTTPS origin explicitly. WebSocket upgrades require that Origin.
- Vite tunnels only `/_transport/ws` to the native host, retaining Host, Origin,
  cookies, and upgrade bytes. Its own HMR connection remains separately owned.

## Selected compatibility contract

Reference: `~/code/bod/snap`, selected revision
`9689a8ed3108f58233721c2000d2b9ea96259fe7`.

| Operation | Carrier | Identity requirement |
| --- | --- | --- |
| `account.create` | HTTP Submit | optional |
| `identity.fetch` | HTTP Query | optional |
| `identity.password.acquire` | HTTP Submit | forbidden |
| `identity.release` | HTTP Submit | required |
| `identity.credentials` | WebSocket Message | required |
| `identity.sessions` | WebSocket Message | required |

Retain path projection, Build negotiation, operation correlation, completion
envelopes, domain failures inside `OperationError.failure`, Approved password
results, void outputs, credential/session summary fields, and `sessionChanged`.
Password bounds use JS UTF-16 length. Emails are trimmed and lowercased.

Cookie names are `authy_session` over HTTP and `__Host-authy_session` for a configured
HTTPS origin, with Path=/, HttpOnly, SameSite=Lax, and matching Max-Age. Duplicate
active cookie names are rejected. Invalid signatures cannot identify a caller.

WebSockets use `/_transport/ws?clientId=...&build=...`, `transport.epoch`, sequence
IDs within that epoch, `transport.ack`, and `transport.complete`. Code 4001 means
the session ended; 4003 means Build mismatch. A stale Build receives its close
frame after upgrade so browser clients can observe the reason.

This read-only Message slice creates a new logical epoch on every physical
attachment. It does not retain detached operations or promise Message mutation
replay. Clients discard incomplete reads, reconnect, and request fresh snapshots.
Full TypeScript Transport retention is outside this selected subset. The actual
TypeScript Passport/Transport/BrowserTransport client is checked against the Rust
host for the selected password/session operations.

## Client lifecycle

Public observations distinguish loading, anonymous, identified, error, and closed
state. Connection status is separate. Rust owns pending commands, identity and
collection observations, read coalescing, five-second read deadlines, reconnect
backoff, and late-result fencing. React owns form fields and rendering. Passwords
never enter an observation.

HTTP and socket generations are separate: a recoverable socket drop cannot erase
an in-flight HTTP command. Recoverable drops reconnect from 250 ms up to 5 seconds.
New epochs refresh credentials and sessions. Revocation clears authenticated
observations immediately. If a release's socket closes before its HTTP response,
the client still waits for the command result, then resolves current identity.
HTTP failure with unknown mutation outcome causes a refetch, never a resend.

Browser Build mismatch requests page reload. The native client terminates its
owning client runtime. Explicit close cancels owned IO/timers and rejects pending
commands. Binding observations are immutable and have stable identity between
notifications. Closing the SDK does not sign out its persisted server session.

## Verification

- `tests/sdk/identity.contract.ts` runs one password/session journey through native
  and browser Rust SDK adapters. It covers duplicate enrollment, invalid password,
  collections, session identity, revocation, sign-in again, and close.
- `tests/sdk/identity-recovery.test.ts` runs the same HTTP failure and close cases
  through native and browser SDKs. Its carrier retains the real server's cookies
  while corrupting completion bodies, and holds responses to exercise cancellation.
  It covers stale initial/Submit Builds, uncertain mutation recovery without replay,
  and final closed observations with a command pending.
- `tests/protocol/authy.test.ts` asserts independent wire examples, cookie projection,
  identity modes, malformed input, Build close codes, sequence admission, and restart
  persistence through real IO.
- `tests/browser/authy.spec.ts` checks the usable packaged UI and development
  WebSocket proxy, page reload, wrong-password feedback, server restart, and remote
  revocation using independent browser cookie jars.
- `scripts/authy-reference.ts` runs the existing TypeScript SDK against Rust.
- `./bin/check` includes these checks plus the existing Healthy, CLI, portability,
  dependency, documentation, and browser suite.

Review record: `docs/reviews/authy-password-sessions.md`.
