# Identity

`snap-identity` owns password credentials, durable sessions and their portable
transport operations. There is no Passport crate. Native crypto lives in
`snap-crypto`; hosts supply time and Store. Access policy and account profiles belong
to their respective consumers.

## Operations and transactions

| Request | Input | Result |
| --- | --- | --- |
| `identity.enroll` | `email`, `password` | New bearer and session summary |
| `identity.login` | `email`, `password` | New bearer and session summary |
| `identity.current` | null, authenticated bearer | Identity ID and absolute expiry |
| `identity.logout` | null, authenticated bearer | null after revocation commits |

These are transport `Request` operations, not connected `Invoke` operations.
Enroll/login take no existing bearer. Each login creates an independent session.
Logout affects only the supplied session. Disconnect leaves the session valid.

The Rust `Identity` methods receive the caller's `&mut Transaction`, so enrollment,
the first session and application records can commit atomically. The operation parser
and dispatcher live in the same portable crate. Hosts return issuance results only
from `Store::run`'s `Committed` value. Database rejection publishes neither a bearer
nor resident state; an unknown commit outcome fences the Store and is not retried.

Store misses remain terminal `StoreMiss` outcomes, never invalid credentials or
execution `Need`. The caller must explicitly arrange loading and submit a new request.
Testy preloads the complete Identity tables at startup. Negative lookups are then
known absence rather than misses. No advisory session cache grants authority.

## Credential and session policy

The initial credential is an ASCII email address with one `@` and nonempty parts,
trimmed and lowercased, at most 254 bytes. Passwords are 8 to 1024 UTF-8 bytes. This
normalization is not email verification. Normalized addresses are unique.

Native hashes use Argon2id with its encoded parameters and a random 128-bit salt.
Bearers contain 256 bits from OS randomness and are encoded as lowercase hex. Only
their SHA-256 digests enter Store. Identity IDs are independently random. Crypto
failures abort the transaction. The deterministic crypto in test support is insecure
and never selected by network hosts.

Sessions expire at `now >= expires`, using host-supplied Unix seconds. The default
lifetime is 30 days; tests select shorter lifetimes and a controlled clock. Expired
rows may remain in storage but grant no authority. Password verification and session
creation run under the same Store exclusion, so verified credentials cannot change
between the check and commit. Synchronous native hashing currently blocks the local
host during the operation. Password change/reset and additional credential types
are not implemented. Authy exposes session summaries and current/others/all
revocation. `Identity::revoke_session` also supports individual owned sessions.
Credential summaries contain labels, never password hashes.

The internal `resolve_digest` method validates a persisted session reference in
the same transaction as protected work. OIDC uses it to bind grants to sessions.
Digest handles are not wire credentials and must never enter client observations.
Authy's signed HttpOnly cookie carries the raw bearer; its Document browser SDK
does not receive that bearer. See [Authy](../apps/authy/CONTRACT.md) for HTTP/OIDC
and cookie behavior. Testy's browser policy below is separate.

## Calculator lifetime

Testy's network hosts select Identity as transport's live authority. Every connected
invocation, executor step and idle sweep revalidates current session authority.
Revocation, expiry or unavailable authority closes the affected connection lifetime.
Unavailable authority does not delete its persisted session; reconnect is explicit.
Different sessions and different tabs get separate calculators, even for one identity.

Connection loss immediately discards the calculator and fails queued/active work.
Late dependency replies and saved execution snapshots cannot restore a retired scope.
Sign-out closes every connection using that session; other sessions continue working.
This policy is stricter than the generic executor's finish-owned-work release policy.

The browser keeps its bearer in tab-scoped session storage and clears it after
confirmed sign-out or invalid-session responses. Reload uses that bearer with a fresh
client ID and calculator. No mutation is automatically replayed after a lost response.
Authentication frames are redacted from the browser wire panel, and Identity operations
never enter retained execution traces. Testy remains a loopback development host with
trusted debugger access, not a public identity deployment.

Persistent application writes must call `Identity::resolve` inside their own Store
transaction. Transport's verified identity alone is not a durable authorization guard.
Calc is ephemeral, so its host checks authority and executes under local host exclusion.

## Database and verification

```sh
./bin/snap migrate --database .snap/testy-identity.sqlite --migrations crates/identity/migrations
./bin/dev

mise exec -- cargo test -p snap-identity
mise exec -- cargo test -p testy-local --features identity --test identity
mise exec -- cargo test -p snap-core-properties --test identity-properties
mise exec -- cargo test -p testy-properties
mise exec -- cargo test -p snap-crypto --test native -- --ignored
./bin/snap test apps/testy native
./bin/check-testy-web
```

`TESTY_DATABASE` overrides the database path. Startup opens an explicitly migrated
database and loads Identity tables; stop the host before running migrations. Credentials
and revocations survive restart. Calculator state and physical attachments do not.

Capability-owned properties model enrollment, login, failed authentication, time,
revocation and issuance failures. Testy's separate property consumer models combined
session/connection/calculator lifetimes. Real crypto, native sockets, browser flows,
SQLite restart and held-work retirement have their own focused tests.
