# Chatty

Chatty uses Authy OAuth and Access-protected Documents for private conversations.
Its portable application is synchronous `no_std` + `alloc`. Native hosts own IO;
the browser uses the Rust Document SDK through Wasm. Updates are pushed, not polled.

## Login and authority

Both apps share `CHATTY_CLIENT_SECRET`, at least 32 bytes. Chatty is the confidential
`chatty` client. Code flow uses PKCE S256, state, nonce and a separate signed
browser-correlation cookie. The RP checks the configured issuer, pinned RS256/JWKS,
audience, time claims, nonce, optional authorized party/access-token hash and
UserInfo subject. Discovery endpoints stay on the issuer origin. HTTP does not
follow redirects or retry exchanges automatically.

The browser receives a signed HttpOnly, SameSite=Lax local session cookie. HTTPS
selects a Secure `__Host-` cookie. Access, refresh and ID tokens remain in private
Store rows. Owner IDs derive from issuer plus case-sensitive subject. Chatty has
no passwords, direct Authy database access or shared Authy bearer.

Application operations use the shared WebSocket dispatcher. It validates identity
and Access before ACK under the application gate. Accepted authority lasts through
completion. WebSockets validate Origin and resolve the session cookie. OAuth and
session bootstrapping remain HTTP; logout requires session-bound `x-snap-csrf`.

Local sessions last at most 30 days. Refresh commits a consumption fence before
HTTP; failed/uncertain exchanges require a fresh login. Restart retires in-flight
exchange authority instead of retrying it. Accepted work can survive an active
refresh while its previous access token remains valid. Authy revocation is observed
on refresh or access expiry, up to ten minutes with Authy's token lifetime.

Logout revokes the local session first and cancels its previous continuation. It
navigates to Authy confirmation using client, registered redirect and single-use
state. ID tokens do not enter browser URLs. The callback checks browser correlation.

## Conversations

Chatty stores and synchronizes messages between clients. It performs no model,
search or file-tool IO. Existing saved replies remain readable.

An owner can create up to 200 conversations, each with at most 200 messages of
32 KiB. `chatty.create` grants its authenticated creator ownership. `chatty.send`
and `chatty.rename` compose the named `send` and `rename` Document mutations in
the dispatcher transaction. `chatty.delete` composes `document.delete`, removing
the conversation from normal loading while retaining stored cleanup state.

Messages record the verified sender. Repeating a message ID with the same sender
and content does not append it twice; different content conflicts. Invocation
retries reuse their ID within a surviving logical connection. A lost logical
lifetime reports unknown outcomes rather than replaying effects as new requests.

Document updates synchronize all accessible active conversations. No separate
HTTP application command routes exist.

## Local operation

From the repository root:

```sh
# Explicit setup: builds, migrations and local client-secret creation if absent.
mise exec -- bun scripts/chatty.ts --migrate
mise exec -- bun scripts/chatty.ts
```

The runner reads `.snap/chatty.env`, with environment overrides. Authy defaults to
`127.0.0.1:3846` and Chatty to `127.0.0.1:3850`; `AUTHY_WEB_ADDR` and
`CHATTY_WEB_ADDR` change loopback ports. Startup never silently migrates or replaces
an occupied listener. Dev supports frontend HMR and native/Wasm replacement;
failed builds retain the previous generation.

Independent `./bin/snap dev apps/chatty` needs `AUTHY_ORIGIN` and the matching
`CHATTY_CLIENT_SECRET`. `./bin/snap build apps/chatty` produces `dist/chatty/chatty`
and adjacent assets. Standalone use:

```sh
SNAP_DATABASE=apps/chatty/.snap/chatty-store.sqlite ./dist/chatty/chatty --migrate
SNAP_DATABASE=apps/chatty/.snap/chatty-store.sqlite ./dist/chatty/chatty
```

`CHATTY_ADDR` selects the native loopback listener. `SNAP_ORIGIN`, `AUTHY_ORIGIN`,
`SNAP_WEB_DIR` select origins and assets. Standalone
database defaults to `.snap/chatty-store.sqlite` relative to cwd; dev uses
`apps/chatty/.snap/chatty-store.sqlite`. Preserve SQLite for sessions, keys and
threads. Old databases are not automatically imported. Workers and public
deployment are not supported targets.

## Verification

```sh
./bin/snap test apps/chatty full
TMPDIR=/tmp/opencode mise exec -- bun test tests/cli/chatty-dev.test.ts
```

Tests use temporary stores and real Authy OAuth. Core tests cover guarded writes,
verified senders, message deduplication, atomic rollback and retained deletion.
Browser tests cover cross-client synchronization, denial before ACK, restart and
logout. The native OAuth gate covers RSA verification.
