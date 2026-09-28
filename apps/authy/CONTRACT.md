# Authy

Authy composes Identity credentials/sessions, Access-protected Document profiles
and a portable OAuth 2.0 / OpenID Connect issuer over one Store. Its supported host
is native SQLite with a browser UI. Workers is not a supported target.

## Accounts and profiles

Enrollment atomically creates the identity, credential, first session, profile and
Access ownership. Email normalization and password policy belong to
[Identity](../../docs/identity.md). Password change/reset, passkeys and email
verification are not implemented. `email_verified` is false.

The browser uses these same-origin HTTP routes:

| Route | Purpose |
| --- | --- |
| `POST /api/signup` | Create an account and first session |
| `POST /api/login` | Create an independent session |
| `GET /api/session` | Current account or null |

Connected account operations use WebSocket invocations: `authy.sessions` and
`authy.credentials` take null and return session summaries and credential labels;
`authy.logout` takes `{scope: "current" | "others" | "all"}`. Logout revokes the
selected sessions durably. The old cookie grants no authority afterward; HTTP
session bootstrapping clears it when the browser checks its session again.

Account responses contain identity and profile IDs, not bearer material. Mutating
HTTP routes require the canonical Origin. Session cookies are signed, HttpOnly,
SameSite=Lax and Path=/. HTTPS selects `__Host-authy_session` with Secure; local
HTTP selects `authy_session`. Invalid signatures and duplicate active cookies do
not authenticate. Sessions have a 30-day absolute lifetime.

Profiles contain name and bio. The Rust Document SDK owns optimistic edits,
ACK-paced submission, revision guards, reconciliation and replication over
`/transport`. The browser owns socket IO and React subscriptions. A profile edit
uses its projected revision; conflicting concurrent edits reject and reconcile.
HTTP account APIs are not a second profile-write path.

The WebSocket host reads the signed cookie when a browser Connect carries an
empty bearer. It validates session authority before ACK under the shared gate.
Accepted authority survives through completion; session changes serialize behind it.
Revocation clears the visible profile and stops authenticated delivery. Ordinary
reconnect recovers a surviving logical connection. Expired connections clear
pending writes and reload a fresh manifest. Closing the SDK does not revoke the
persisted session. Invocation channels retry with the same ID until ACK and recover
pending outcomes on a surviving reconnect. A fresh lifetime reports unknown outcomes.

## OAuth and OIDC

Sign-in, signup, consent, logout confirmation and browser protocol errors share
`web/auth-ui.tsx` and the tokens in `web/style.css`. The web build renders the
script-free protocol views into `auth-pages.json`; the native host loads that
artifact and fills escaped request data. Change the shared components to update
both render paths. Consent shows the registered application origin and explains
each permission. JSON protocol errors remain JSON for non-browser clients.

| Endpoint | Behavior |
| --- | --- |
| `/.well-known/openid-configuration` | Issuer metadata |
| `/oauth/jwks` | Public RS256 key |
| `/oauth/authorize` | Code authorization and explicit consent |
| `/oauth/resume` | Resume after password authentication |
| `/oauth/token` | Code redemption and refresh rotation |
| `/oauth/userinfo` | GET/POST with an access-token Bearer header |
| `/oauth/revoke` | Revoke a token's grant family |
| `/oauth/logout` | RP-initiated logout with browser confirmation |

Chatty is registered as `chatty`. `CHATTY_ORIGIN` defaults to
`http://127.0.0.1:3850`; exact redirects are `/auth/callback` and
`/auth/logged-out` at that origin. Configuring `CHATTY_CLIENT_SECRET` with at
least 32 bytes selects confidential `client_secret_basic`; otherwise it is a
public PKCE client.

Every code flow requires PKCE S256. Scopes are `openid`, `profile` and `email`.
Every authorization displays consent. `prompt=none` returns an interaction error;
`prompt=login`, `select_account` and stale `max_age` require fresh authentication.
Codes expire after 60 seconds, continuations after five minutes, access/ID tokens
after ten minutes, and refresh families after 30 days. Refresh authority depends
on the originating Identity session. There is no `offline_access`.

Issuer tables retain digests and consumed-token lineage, not raw codes or access/
refresh tokens. Code/refresh replay revokes the grant family. Session validation,
fresh profile claims and token issuance share the caller's transaction. Tokens
leave the host only after durable commit. RS256 and cookie keys persist in Store;
JWKS exposes only public parameters. Native signing currently runs synchronously.
Unavailable profile timestamps are omitted rather than fabricated.

Consent/logout POSTs require the exact issuer Origin and a session-bound handle.
Their pages use `Referrer-Policy: same-origin` and restrict CSP form navigation to
the issuer and configured RP origins. The issuer still checks exact registered
redirect URIs. Logout accepts relevant signed expired ID-token hints and revokes
the current session after explicit confirmation. Applications enforce their own
local-session lifetimes.

Dynamic registration, request objects, implicit/hybrid flows, encrypted ID tokens,
key rotation, introspection and front/back-channel logout are not implemented.

## Run and verify

From the repository root, after installing the tools described in the README:

```sh
mise exec -- cargo build -p authy-native
SNAP_DATABASE=apps/authy/.snap/authy-store.sqlite target/debug/authy --migrate
./bin/snap dev apps/authy
./bin/snap build apps/authy
SNAP_DATABASE=apps/authy/.snap/authy-store.sqlite ./dist/authy/authy
./bin/snap test apps/authy full
TMPDIR=/tmp/opencode mise exec -- bun test tests/cli/authy-dev.test.ts
```

Dev opens `http://127.0.0.1:3846`; `AUTHY_WEB_ADDR` overrides it. Vite owns frontend
HMR; successful Rust builds replace both native host and Wasm SDK, then reload.
Failed builds retain the previous generation. Persisted accounts/profiles survive.
The package places web assets beside `dist/authy/authy`.

`SNAP_DATABASE` selects the database. Dev defaults to
`apps/authy/.snap/authy-store.sqlite`; standalone defaults to
`.snap/authy-store.sqlite` relative to its working directory. Startup verifies and
loads an explicitly migrated database. Close the host before migrations. There is
no automatic import from old Passport databases; preserve those files separately.

`AUTHY_ADDR` sets the native loopback listener. `SNAP_ORIGIN` sets the canonical
browser/issuer origin; dev supplies its public Vite origin. `SNAP_WEB_DIR` overrides
assets. `SNAP_SESSION_KEY` may override the persisted cookie key with at least
32 bytes; changing it invalidates existing cookies. Preserve SQLite to retain
credentials, sessions and signing identity. Local tests do not establish a public
deployment or OIDC certification.

Portable tests cover account atomicity and issuer state transitions. Native tests
verify real HTTP, independent RSA verification, restart, replay and revocation.
Browser journeys exercise profile replication, login, consent and forced login.
The disposable-source dev gate covers rebuilds, HMR and owned-process shutdown.
