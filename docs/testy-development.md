# Testy development controls

After the [Identity database setup](../README.md#run-testy), start with `./bin/snap dev apps/testy`
and open `http://127.0.0.1:3848`. `/` is the mini-app
launcher, `/healthy` exercises anonymous health requests, and `/calc` bootstraps
a login-required, connection-owned calculator. Browser SDK requests and tool requests share one
transport/execution instance. The debugger connection stays responsive while the
application-wide execution gate is held.
Identity requests run synchronously outside the calculator stepping queue, under
host exclusion. Password hashing briefly occupies that host; it cannot be stepped.
Identity requests/results are excluded from retained traces. Socket loss, logout or
expiry discards Calc and fails its unfinished work even while the debugger is held.

## Build and reload

`snap dev` discovers `snap.toml` from the current directory, or takes an explicit
app directory. The checkout wrapper runs from the repository root, so use
`./bin/snap dev apps/testy` (also available as `./bin/dev`). Ctrl-C stops the watcher,
Vite, the native host and any compiler children.

Vite serves frontend source with React refresh and CSS hot replacement. Rust source,
Cargo manifests and lockfile changes under Testy, `crates/` and `platforms/` trigger
a debounced build of the native host and Wasm SDK. Builds run serially into private
generations. A compile failure leaves the previous host and bindings active; a later
edit retries. Changes arriving during compilation supersede that build. After a
successful build the host restarts and browsers reload. If host startup fails, the
driver restarts the previous executable. Rust replacement has a short outage and
loses calculators and debugger state. Persisted accounts/sessions survive, and the
browser retains its tab-scoped bearer. This is process replacement, not in-process
Rust code hot swapping.

`TESTY_WEB_ADDR` sets Vite's public loopback address (default `127.0.0.1:3848`). The
Rust host uses a private loopback port; Vite proxies transport and debugger HTTP/WS
after checking the public Host/Origin. An occupied public port fails startup.
`TESTY_DATABASE` selects an already-migrated database. Stop development before
running migrations. Changes to the dev/build scripts require restarting `snap dev`.

`./bin/snap build apps/testy` (or `./bin/build`) produces `dist/testy-web` and `dist/web`.
The packaged executable serves adjacent assets without Vite. It uses `TESTY_WEB_ADDR`,
`TESTY_WEB_DIR` and `TESTY_DATABASE` as documented in the README.

## Observe and control

`GET /__dev` returns committed `states`, `active`, `queued`, `peers`, stepping
mode, selected program and the last 256 trace records. Each trace record has an
increasing sequence number, and records submitted invocations and resolved inputs
for replay. An active call includes its ticket, scope, operation,
input, acceptance status, missing dependency and already supplied inputs. Private
attempt data is discarded on return and cannot be inspected between entries.
The execution desk sends dependency JSON as text through its Rust binding, which
also formats records and trace text for display. Signed-64-bit values remain exact
even outside JavaScript's safe-integer range; the HTTP interface still uses JSON numbers.

## Live debugger channel

Connect to `ws://127.0.0.1:3848/__dev/ws` for live observation and control. This is
independent of application WebSockets at `/transport`: opening or closing a
debugger creates no calculator session and never resets state or resumes held work.
The execution desk uses this channel and makes no periodic HTTP requests.

The host immediately sends a full inspection report:

```json
{"type":"state","revision":"0","state":{"manual":false,"breakpoint":false,"program":"standard","snapshot":false,"active":null,"queued":[],"releases":0,"states":[],"peers":[],"trace":[]}}
```

It pushes another report when inspection changes, including changes caused by
application traffic, other debuggers, HTTP commands or connection expiry. Each
report replaces the previous one. Revisions are increasing decimal strings scoped
to the host process, not snapshot versions. Slow subscribers can skip revisions;
the host retains only the latest full report rather than buffering a stream per
debugger. The trace within it still holds only the last 256 observations. This
channel does not promise delivery of every intermediate execution event.

Send the same controls listed below inside an envelope with a unique string ID:

```json
{"id":"step-1","control":{"action":"step"}}
```

The host executes commands from that socket in receive order and returns exactly
one correlated result for each valid envelope while the connection remains live:

```json
{"type":"result","id":"step-1","result":{"manual":true,"active":null,"queued":[],"states":[],"trace":[]}}
```

The `result` is the same value returned by HTTP controls, with the full inspection
fields omitted in the example above. A failure instead has `"error":"..."` and
no `result`. Invalid control actions also receive a correlated error. State reports
may arrive between results. A step result confirms that host step has finished;
it does not imply that the application operation has completed.

IDs must be nonempty strings of at most 128 UTF-8 bytes. Malformed envelopes,
non-text commands or input messages larger than 64 KiB close the connection.
IDs correlate responses only; sending an ID again executes a new command. On
connection loss, a submitted command's outcome can be unknown. Inspect the current
state before deciding what to do next; do not blindly replay mutations.

The desk reconnects with backoff from 250 ms to four seconds and gets a fresh
report. It marks retained state as stale, disables its controls while disconnected,
and rejects pending commands rather than replaying them. A ten-second response
timeout also closes its socket. Host writes time out after five seconds without
holding the application lock, so a stalled debugger cannot block execution.

Both debugger and HTTP endpoints require the printed loopback Host address and,
for browser connections, a matching Origin. Tools can omit Origin. Explicit `open`
controls allocate tool-owned application peers; these require `drop` even if the
debugger socket that created them closes.

## HTTP controls and actions

`POST /__dev` accepts JSON with an `action` field. Success returns inspection unless
noted below. Failed controls return HTTP 409 with `{"error":"..."}`. The listener
requires a loopback address and matching Host/Origin headers. Use its printed URL.

| Action | Other fields | Effect |
| --- | --- | --- |
| `open` | none | Allocate a tool-owned physical peer; returns `{"peer":number}` |
| `send` | `peer`, `command` | Submit a transport `Command` using that peer |
| `drain` | `peer` | Take ordered observations; returns `{"responses":[...]}` |
| `drop` | `peer` | Discard that calculator and fail its unfinished work |
| `mode` | `manual`: boolean | Hold host stepping or resume automatic execution/input resolution |
| `breakpoint` | `enabled`: boolean | Stop after acceptance, before handler entry |
| `step` | none | Enter manual mode and perform one executor observation |
| `supply` | `ticket`, `key`, `value` | Resolve exactly the outstanding read |
| `fail` | `ticket`, `key` | Fail that read with `Unavailable` |
| `snapshot` | none | Save idle committed state in the host's one snapshot slot |
| `restore` | none | Restore the saved state at an idle gate with the same live scope IDs |
| `replace` | `program` | Select `standard` or `double-add` at an idle gate |

Manual mode continues accepting submissions. An operation waiting for a dependency
needs supply/failure before another step can advance it. Run mode resolves reads
through the configured host resolver. Testy's resolver supplies ceiling 1000.
The breakpoint remains armed until disabled, including for `calc.inspect` queries.

`step` is an execution-boundary step, not a source-line debugger. It can report
acceptance, a dependency request, or completion. It may also drain queued lifecycle
releases. Snapshot/restore and replacement reject outstanding work. To revert a
request, save before issuing it, finish or fail it, then restore. Sockets, clocks,
trace entries and observations already delivered to clients are not rewound.

`double-add` replaces addition with `a + 2*b`; both variants are compiled into the
host. Loading newly compiled modules remains a later implementation. Replay uses
a fresh invocation ID and explicit dependency values. Restore changes authoritative
host state; use the calculator's Refresh button to update the screen's last result.

Tool-owned application peers have no socket to detect abandonment. Drain responses and explicitly
drop them when done. The host bounds physical peers to 128 and buffers at most one
outstanding operation per peer. It rejects further commands if 64 response frames
await draining. Browser peers are detached automatically on socket loss.

## Tool-driven replay

This example creates a calculator at 42, stops after acceptance, observes rollback
on a late read, supplies the input, restores 42, replaces code and replays +10 to
produce 62. Run it against an otherwise idle host. The snapshot captures all live
scopes, including any browser calculators.

```python
import json, os
from urllib.request import Request, urlopen

base = "http://127.0.0.1:3848/__dev"
def control(action, **fields):
    body = json.dumps({"action": action, **fields}).encode()
    with urlopen(Request(base, body, {"Content-Type": "application/json"})) as r:
        return json.load(r)

peer = control("open")["peer"]
sequence = 1
def invoke(operation, value=None):
    global sequence
    sequence += 1
    return control("send", peer=peer, command={"Invoke": {
        "id": sequence, "operation": operation, "input": value}})

try:
    control("send", peer=peer, command={"Request": {"bearer": None,
        "invocation": {"id": 1, "operation": "identity.login", "input": {
            "email": os.environ["TESTY_EMAIL"], "password": os.environ["TESTY_PASSWORD"]}}}})
    replies = control("drain", peer=peer)["responses"]
    bearer = replies[-1]["Events"][0]["Completed"]["outcome"]["Ok"]["bearer"]
    control("send", peer=peer, command={"Connect": {
        "bearer": bearer, "client_id": "agent-demo"}})
    control("drain", peer=peer)
    invoke("calc.start")
    control("drain", peer=peer)
    invoke("calc.add", 42)
    control("drain", peer=peer)
    control("snapshot")
    control("breakpoint", enabled=True)
    invoke("calc.add_checked", 10)
    held = control("step")
    print("Committed while waiting:", held["states"])
    pending = held["active"]
    control("supply", ticket=pending["ticket"], key=pending["waiting"], value=1000)
    print("First commit:", control("step")["states"])
    control("drain", peer=peer)
    control("restore")
    control("replace", program="double-add")
    invoke("calc.add", 10)
    control("step")
    print("Replacement commit:", control("step")["states"])
    control("drain", peer=peer)
finally:
    control("breakpoint", enabled=False)
    control("mode", manual=False)
    control("send", peer=peer, command="Close")
    control("drop", peer=peer)
    control("replace", program="standard")
```

Tools can also control a browser's in-flight request without allocating their own
peer. Inspect `active`, then step/supply it through the same HTTP interface. The
browser receives ACK and completion on its original WebSocket.
