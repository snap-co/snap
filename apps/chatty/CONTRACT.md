# Chatty

Chatty is a personal assistant with Authy sign-in and private persistent threads.
Its portable Rust application runs on native and local Workers. The browser uses
ordinary same-origin JSON endpoints and polls persisted turn progress every 700 ms.
Chatty's browser bundle has no Passport or model-provider credentials.

## Login and account ownership

Chatty is Authy's statically registered confidential `chatty` client. Both apps must
receive the same `CHATTY_CLIENT_SECRET`, at least 32 characters. Login uses code +
PKCE S256 with separate state, nonce and browser-correlation cookie. The server
validates the configured issuer, pinned RS256 signature/JWKS, audience, time claims,
nonce, optional authorized party/access-token hash, and UserInfo subject agreement.
Issuer metadata endpoints must stay on the configured issuer origin. It does not
discover an issuer supplied by a browser or token.

The browser holds an opaque, signed, HttpOnly, SameSite=Lax local session cookie.
OAuth access/refresh/ID tokens stay in server-side Store. Session rows are keyed by
bearer digest. Owner IDs derive from the configured issuer and case-sensitive
subject. Every thread operation checks current owner/session authority. Mutations
also require the exact Origin and a session-bound CSRF header. There is no CORS API.

Local sessions last at most 30 days. Access tokens refresh before expiry, serialized
through a Store guard. Failed or interrupted refresh requires a new login; a lost
exchange is never replayed. Authy revocation is observed on the next refresh, so an
independent Chatty session may remain usable for up to ten minutes. Chatty logout
first deletes its local session, then directs the browser to Authy confirmation.
It cancels the current browser's outstanding login attempt. Logout callbacks require
their own single-use state and browser correlation.

HTTP on loopback/LAN/tailnet is an explicit development mode. Set HTTPS origins in
a deployment so cookies become Secure. General OIDC conformance and cloud deployment
are not established by local tests.

## Threads and generation

Each owner can keep 200 threads, each with up to 200 turns. Messages are limited to
32 KiB. Threads have an editable title and reasoning effort. Each turn persists the
user message, display answer, optional provider summary, opaque provider output,
tool calls/results, usage and completion status. Opaque reasoning never enters the
browser response. No raw thinking transcript is claimed.

`/api/send` requires a per-message request ID. Acceptance commits the user message
and active-turn fence before starting retained work, then returns 202. Repeating the
same ID/message returns that turn; changing its message is a conflict. A thread
admits one generation at a time, and a host instance admits four. Generation uses
at most five model steps and eight tool calls, with 8192 output tokens per step.
These are execution bounds, not a separate spending approval policy.

The default provider is OpenCode Go's `muse-spark-1.3-contributor` at
`https://opencode.ai/zen/go/v1/responses`. `CHATTY_MODEL` and
`CHATTY_MODEL_ENDPOINT` are server configuration overrides. Requests send
`store:false`, encrypted reasoning inclusion and a stable thread session header.
Model streams are consumed incrementally; terminal output preserves provider item
ordering, encrypted reasoning, assistant phase and tool call/result pairing.
Summaries display only when supplied. Usage separates reasoning/output/input/cache
counts. Contributor data handling follows the chosen provider's terms.

Context retains the most recent complete turns up to a 192 KiB serialized budget.
It drops whole older turns and shows the omitted count. Failed, cancelled and
interrupted turns stay visible but are omitted from subsequent model context.
Context overflow after tool results ends that reply with an explicit error.

Accepted work survives HTTP observer loss in the current host. It is not a durable
workflow: process/isolate loss marks running turns interrupted at next startup and
never repeats model requests or file writes. Cancel/delete fences reject later
progress. Already-started external work may still finish and be billed; stopping
the UI does not prove remote cancellation. Logout prevents further commits and
releases the old turn's active slot. Network failures and provider rate limits are
visible errors, without automatic retries or tool re-execution.

## Tools

Native exposes list/read/write tools in a dedicated per-owner workspace. cap-std
confines path resolution to that directory. Paths are relative, at most eight
components; files are UTF-8 and at most 64 KiB, with 200 files per workspace. Writes
replace via a temporary file and rename. Tools cannot run shell commands. Local
administrators remain trusted to control workspace roots and file permissions.

Exa search is available when `EXA_API_KEY` is set. Each call requests five results,
with bounded excerpts and source URLs persisted in the tool result. The assistant
is instructed to cite those URLs. Retrieved text is untrusted source material.
Workers currently omits file tools; account/login/chat/search use the shared core.

## Hosts and development

Run the native pair from the repository root:

```sh
mise exec -- bun scripts/chatty.ts
```

The runner builds both apps, loads `.snap/chatty.env`, creates the shared client
secret there if absent, and passes secrets through process environments. It binds
Authy to 3846 and Chatty to 3850. `--no-build` reuses artifacts. For another device:

```sh
CHATTY_HOST=achilles CHATTY_BIND=0.0.0.0 mise exec -- bun scripts/chatty.ts
```

It does not replace occupied ports. Stop the existing process before restarting.
Native data lives in `apps/authy/.snap/authy.sqlite`,
`apps/chatty/.snap/chatty.sqlite`, and `apps/chatty/.snap/files`.
`SNAP_DATABASE`, `CHATTY_FILES`, `SNAP_ORIGIN` and `AUTHY_ORIGIN` select standalone
native configuration. The runner also accepts `AUTHY_PORT` and `CHATTY_PORT`.

Chatty uses a plain React HTTP client, rather than a dummy WASM binding for the
existing Snap Protocol client. `bun scripts/build-chatty.ts` writes its browser
bundle to `apps/chatty/.snap/web`; set `SNAP_WEB_DIR` to that directory for the
standalone native binary. The app's `snap.toml` selects server/check commands; the
pair runner owns this browser build and launch procedure.

The Workers composition keeps one realm's sessions and threads in one SQLite
Durable Object. It uses an `AUTHY` service binding for server-to-server OIDC calls.
For local mixed-host tests only, `CHATTY_AUTHY_HTTP=1` selects ordinary HTTP to the
configured issuer. Set matching origins and client secrets on both Workers; store
keys in ignored `.dev.vars`, never in Wrangler config. Model/search keys belong only
to Chatty. Build browser assets before Wrangler starts. Work remains active while
the object has pending IO, but runtime eviction/reset still has uncertain outcomes.

Tests use fresh fixture data and no paid keys by default. `bin/check` runs the
native/workerd thread contract, independent RP validation/refresh tests and both
browser journeys. Live Go/Exa checks are deliberate one-off integrations.
