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

Each protected write rechecks authority under the serialized Store gate. Mutating
HTTP calls require canonical Origin and session-bound `x-snap-csrf`. WebSockets
validate Origin and resolve the cookie in the same host.

Local sessions last at most 30 days. Refresh commits a consumption fence before
HTTP; failed/uncertain exchanges require a fresh login. Restart retires in-flight
exchange authority instead of retrying it. Accepted work can survive an active
refresh while its previous access token remains valid. Authy revocation is observed
on refresh or access expiry, up to ten minutes with Authy's token lifetime.

Logout revokes the local session first and cancels its previous continuation. It
navigates to Authy confirmation using client, registered redirect and single-use
state. ID tokens do not enter browser URLs. The callback checks browser correlation.

## Threads and external work

Each owner can keep 200 threads with 200 turns each; messages are at most 32 KiB.
The `rename` Document mutation updates title/effort optimistically. Creation,
deletion, message acceptance and cancellation are application HTTP operations.

`/api/send` requires a request ID. Message, active-turn fence and request receipt
commit before the host starts work. Repeating the same ID/message returns its turn
without IO; changing the message conflicts. One turn per thread and four model
tasks per host are admitted. A cancelled task retains its host IO permit until exit.

Generation runs at most five model steps and eight tool calls, with 8192 output
tokens per step. The default provider is OpenCode Go's `muse-spark-1.3-contributor`
at `https://opencode.ai/zen/go/v1/responses`. Server settings `CHATTY_MODEL` and
`CHATTY_MODEL_ENDPOINT` override these. Requests use `store:false`, encrypted
reasoning inclusion and a stable thread session header.

Streaming progress publishes display text, explicit provider summaries, tool
results, usage and completion through Documents. Opaque provider output stays in
private Store rows. Provider ordering, assistant phase and tool call/result pairing
survive subsequent requests. No raw thinking transcript is exposed.

Context retains complete recent turns within 192 KiB, dropping whole older turns
with a visible omitted count. Failed/cancelled/interrupted turns stay visible but
do not become model context. Expanded context after tools is bounded to 320 KiB;
display text plus summaries are bounded to 512 KiB.

Each progress/result publication checks current session, owner and the accepted
turn fence. Cancel/delete/logout reject late results. Retained work survives HTTP
observer loss. Startup marks unfinished turns interrupted and never repeats model
requests or file writes. Already-started remote work may still finish. Errors are
visible without automatic retry.

## Tools

File tools list/read/write a dedicated per-owner workspace. cap-std confines path
resolution. Paths are relative with at most eight components; files are UTF-8,
at most 64 KiB, with at most 200 files. Writes use a temporary file and rename.
Tools do not run shell commands. Local administrators remain trusted.

`EXA_API_KEY` enables search with five results, bounded excerpts and source URLs.
The model is instructed to cite them and treat retrieved text as untrusted. Tools
run outside Store after progress commits. A crash after an external write records
uncertainty rather than repeating the tool.

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
`SNAP_WEB_DIR` and `CHATTY_FILES` select origins, assets and file storage. Standalone
database defaults to `.snap/chatty-store.sqlite` relative to cwd; dev uses
`apps/chatty/.snap/chatty-store.sqlite`. Preserve SQLite for sessions, keys and
threads. Old databases are not automatically imported. Workers and public
deployment are not supported targets.

## Verification

```sh
./bin/snap test apps/chatty full
TMPDIR=/tmp/opencode mise exec -- bun test tests/cli/chatty-dev.test.ts
# Explicit paid-provider gate, using configured model and Exa keys:
TMPDIR=/tmp/opencode mise exec -- bun scripts/check-chatty-live.ts
```

Default tests use temporary stores, real Authy OAuth and a deterministic streaming
provider. Core tests cover rollback, deduplication, private context and lifecycle
fences. Native gates cover file confinement and RSA verification. Browser tests
cover Document edits, streaming, tools, isolation, restart and logout. The live
gate requires an actual model answer citing a returned search source.
