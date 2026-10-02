// Dependency stimuli for the Rust-owned browser/client contracts. The classes
// under test are the production TypeScript implementations, not Rust replicas.
import { BrowserRuntime, type Binding, type Publication } from "../runtime";
import { Invocations } from "../../../crates/platform/document-host/client";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
}
class Socket {
  readyState = 1;
  sent: string[] = [];
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onopen = null; onerror = null;
  send(frame: string) { this.sent.push(frame); }
  close() { this.readyState = 3; this.onclose?.(); }
  receive(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }); }
}
function binding(free = () => {}) {
  return {
    connect: () => "", invoke: () => "", free,
    receive: (frame: string) => JSON.stringify({ ready: JSON.parse(frame).manifest === true, send: [] }),
  };
}
function fixture(fetch: () => Promise<{ id: string } | null>, create?: () => Promise<Binding>) {
  const sockets: Socket[] = [], publications: (Publication | null)[] = [];
  let created = 0, freed = 0, sequence = 0;
  const runtime = new BrowserRuntime({
    identity: { fetch }, key: account => account.id,
    create: async () => {
      created++;
      return create ? create() : {
        ...binding(() => { freed++; }),
        invoke: (operation: string, input: string) => JSON.stringify({ Invoke: { id: ++sequence, operation, input: JSON.parse(input) } }),
      };
    },
    decode: raw => JSON.parse(raw) as Publication,
    publish: result => publications.push(result),
    transport: { connect: () => { const socket = new Socket(); sockets.push(socket); return socket as unknown as WebSocket; } },
  });
  return { runtime, sockets, publications, get created() { return created; }, get freed() { return freed; } };
}
Object.assign(window, { snapClientFixture: { fixture, deferred, binding, Invocations } });
