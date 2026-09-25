# Testy development controls

Start with `./bin/dev` and open `http://127.0.0.1:3848`. `/` is the mini-app
launcher, `/healthy` exercises anonymous health requests, and `/calc` bootstraps
a connection-owned calculator. Browser SDK requests and HTTP tool requests share
one transport/execution instance and its application-wide gate.

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

`POST /__dev` accepts JSON with an `action` field. Success returns inspection unless
noted below. Failed controls return HTTP 409 with `{"error":"..."}`. The listener
requires a loopback address and matching Host/Origin headers. Use its printed URL.

| Action | Other fields | Effect |
| --- | --- | --- |
| `open` | none | Allocate a tool-owned physical peer; returns `{"peer":number}` |
| `send` | `peer`, `command` | Submit a transport `Command` using that peer |
| `drain` | `peer` | Take ordered observations; returns `{"responses":[...]}` |
| `drop` | `peer` | Detach the tool peer; owned work continues |
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

HTTP peers have no socket to detect abandonment. Drain responses and explicitly
drop them when done. The host bounds physical peers to 128 and buffers at most one
outstanding operation per peer. It rejects further commands if 64 response frames
await draining. Browser peers are detached automatically on socket loss.

## Tool-driven replay

This example creates a calculator at 42, stops after acceptance, observes rollback
on a late read, supplies the input, restores 42, replaces code and replays +10 to
produce 62. Run it against an otherwise idle host. The snapshot captures all live
scopes, including any browser calculators.

```python
import json
from urllib.request import Request, urlopen

base = "http://127.0.0.1:3848/__dev"
def control(action, **fields):
    body = json.dumps({"action": action, **fields}).encode()
    with urlopen(Request(base, body, {"Content-Type": "application/json"})) as r:
        return json.load(r)

peer = control("open")["peer"]
sequence = 0
def invoke(operation, value=None):
    global sequence
    sequence += 1
    return control("send", peer=peer, command={"Invoke": {
        "id": sequence, "operation": operation, "input": value}})

try:
    control("send", peer=peer, command={"Connect": {
        "bearer": "testy-private-fixture-token", "client_id": "agent-demo"}})
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
