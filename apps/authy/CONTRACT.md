# Authy compatibility contract

Authy selects the `user` identity kind and `account.create`; Passport owns shared
password/session behavior. It supports account creation, password sign-in, sign-out,
identity recovery, credential/session reads, reconnect, and session revocation.
Password reset/change, fresh assurance, passkeys, OAuth, and multi-realm federation
are outside this slice. See [architecture](../../ARCHITECTURE.md) for composition.

## Persistence and deployment

`SNAP_DATABASE` selects SQLite, default `.snap/authy.sqlite` under the working
directory. Dev runs in the app directory. Accounts, password hashes, sessions, and
the signing key survive rebuilds/restarts. New database files use mode 0600 on Unix.

Passwords use Argon2id; salts and session tokens use OS randomness. Only token
digests enter session storage. HMAC-SHA256 signs cookies using a key persisted in
SQLite unless `SNAP_SESSION_KEY` supplies an explicit key of at least 32 bytes.
Preserve that key and the database across deployments. Sessions last 30 days.

`SNAP_ORIGIN` is the canonical public origin. Dev supplies its public frontend
origin unless overridden. Standalone hosts default to the resolved listen address;
TLS/proxy deployments must supply the public HTTPS origin. WebSocket upgrades
require that Origin. Dev proxies only `/_transport/ws` to the backend, preserving
Host, Origin, cookies, and upgrade bytes separately from Vite HMR.

## Authority and accepted work

Credential claims and the first session commit together; duplicate normalized
emails fail. Session creation rechecks the credential/hash used for password
verification. Each operation resolves authority; protected reads and revocation
recheck it transactionally. Sockets expire and terminate after local revocation.

Accepted writes may finish after an HTTP observer disappears. Admission capacity
remains held until completion. Clients never automatically retry mutations. The
host admits 64 operations and at most 128 live sockets, bounds HTTP bodies and
socket messages to 64 KiB, and applies write deadlines.

## Wire subset

Reference revision and checkout are recorded in [ARCHITECTURE.md](../../ARCHITECTURE.md).

| Operation | Carrier | Identity |
| --- | --- | --- |
| `account.create` | HTTP Submit | optional |
| `identity.fetch` | HTTP Query | optional |
| `identity.password.acquire` | HTTP Submit | forbidden |
| `identity.release` | HTTP Submit | required |
| `identity.credentials` | WebSocket Message | required |
| `identity.sessions` | WebSocket Message | required |

Preserve path projection, Build negotiation, operation correlation, completion
envelopes, failures inside `OperationError.failure`, Approved password results,
void outputs, credential/session summary fields, and `sessionChanged`. Password
bounds use JS UTF-16 length; emails are trimmed and lowercased.

Cookies use `authy_session` for HTTP and `__Host-authy_session` for HTTPS, with
Path=/, HttpOnly, SameSite=Lax, matching Max-Age, and Secure for HTTPS. Duplicate
active cookie names are rejected; invalid signatures cannot identify a caller.

WebSockets use `/_transport/ws?clientId=...&build=...`, `transport.epoch`, sequence
IDs within that epoch, `transport.ack`, and `transport.complete`. Close code 4001
ends a session; 4003 indicates Build mismatch. Stale-Build sockets upgrade before
receiving their close frame so browsers can observe the reason.

Each physical attachment starts a new logical epoch. Clients discard incomplete
reads and request fresh snapshots after reconnect. Detached-operation retention
and Message mutation replay are outside this subset.

## Client lifecycle

Observations distinguish loading, anonymous, identified, error, and closed state;
connection status is separate. Rust owns pending commands, observations, coalesced
reads, five-second read deadlines, reconnect backoff, and late-result fencing.
Passwords never enter observations. React owns forms and rendering.

HTTP and socket generations are independent. Recoverable socket drops cannot
erase in-flight HTTP commands; reconnect backs off from 250 ms to 5 seconds.
Revocation clears authenticated observations immediately. If release closes the
socket before its HTTP response, the client waits for that result, then resolves
identity. An unknown mutation outcome triggers a refetch, never a resend.

Build mismatch requests browser reload or terminates the native client runtime.
Explicit close cancels owned IO/timers and rejects pending commands. Observations
are immutable with stable identity between notifications. SDK close does not sign
out the persisted server session.

Shared native/browser assertions live in `tests/sdk/identity.contract.ts` and
`identity-recovery.test.ts`; wire/migration assertions live in `tests/protocol`.
`tests/browser/authy.spec.ts` covers the UI and development proxy.
`scripts/authy-reference.ts` checks the selected TypeScript SDK against Rust.

## Workers host

`workers/` composes the same Passport provider and browser client with
`snap-workers`. One `SNAP_REALM` identifies one SQLite-backed Durable Object.
That object owns credential uniqueness, sessions, signing-key persistence, and
socket revocation for the realm. NoCache remains selected. Changing the realm or
Durable Object namespace selects different data. Native SQLite files are not
automatically imported into Workers storage.

The host uses local Rust futures and Workers bindings. Password hashing retains
the native Argon2id policy but executes synchronously in Wasm; it blocks that
isolate while hashing and requires an adequate deployment CPU/memory budget.
The local workerd tests prove functionality, not edge throughput or plan capacity.

Sockets use hibernation attachments to retain the token, lease, Build, and receive
fence across object eviction. Reconstructing a host with a different Build closes
old attachments with 4003. Each new physical connection still receives a fresh
epoch. Accepted operations are bounded at 64, sockets at 128, and pending frame
callbacks at 64. HTTP bodies and incoming text frames are bounded at 64 KiB.
Workers queues outgoing frames through its WebSocket API; it does not expose the
native host's physical-write deadline. Expiry rejects operations against current
authority; idle socket closure uses Durable Object alarms and their scheduling
latency. These are host delivery differences, not extended session validity.

`tests/protocol/authy.test.ts`, `tests/protocol/carriers.test.ts`, and the Authy
browser journey run against both native and Workers hosts. The shared Store
contract also runs inside workerd, including rollback, coherent reads, concurrent
claims, advisory caches, binary values, and full-width integers. Tests use temporary
storage and exercise process restart. They do not require Cloudflare credentials.
