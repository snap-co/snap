import { expect, test } from "bun:test";
import { BrowserRuntime, type Binding, type Publication } from "./runtime";

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; }
class Socket {
  readyState = 1;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onopen = null; onerror = null;
  send() {}
  close() { this.readyState = 3; this.onclose?.(); }
  receive(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }); }
}
function fixture(fetch: () => Promise<{ id: string } | null>, create?: () => Promise<Binding>) {
  const sockets: Socket[] = [], publications: (Publication | null)[] = [];
  let created = 0, freed = 0;
  const runtime = new BrowserRuntime({
    identity: { fetch }, key: account => account.id,
    create: async () => { created++; return create ? create() : {
      connect: () => "", invoke: () => "", free: () => { freed++; },
      receive: frame => JSON.stringify({ ready: JSON.parse(frame).manifest === true, send: [] }),
    }; },
    decode: raw => JSON.parse(raw) as Publication,
    publish: result => publications.push(result),
    transport: { connect: () => { const socket = new Socket(); sockets.push(socket); return socket as unknown as WebSocket; } },
  });
  return { runtime, sockets, publications, get created() { return created; }, get freed() { return freed; } };
}
test("anonymous startup opens no socket and failed Identity is retryable, not anonymous", async () => {
  let failed = true;
  const f = fixture(async () => { if (failed) throw new Error("offline"); return null; });
  try {
    await expect(f.runtime.resolve()).rejects.toThrow("offline");
    expect(f.runtime.getSnapshot().phase).toBe("error");
    failed = false; await f.runtime.refresh();
    expect((await f.runtime.resolve()).phase).toBe("anonymous");
    expect(f.sockets).toHaveLength(0);
  } finally { f.runtime.close(); }
});
test("router resolution during acquisition creates only one binding and waits for an empty manifest", async () => {
  const binding = deferred<Binding>();
  const f = fixture(async () => ({ id: "alice" }), () => binding.promise);
  try {
    const acquire = f.runtime.replace({ id: "alice" });
    const route = f.runtime.resolve();
    await Promise.resolve(); await Promise.resolve();
    expect(f.created).toBe(1);
    binding.resolve({ connect: () => "", invoke: () => "", free() {}, receive: frame => JSON.stringify({ ready: JSON.parse(frame).manifest === true, send: [] }) });
    await acquire;
    f.sockets[0]!.receive({ Attached: { resumed: false } });
    expect(f.runtime.getSnapshot().phase).toBe("loading");
    f.sockets[0]!.receive({ manifest: true, documents: [] });
    expect((await route).phase).toBe("ready");
    expect(f.sockets).toHaveLength(1);
  } finally { binding.resolve({ connect: () => "", invoke: () => "", receive: () => "", free() {} }); f.runtime.close(); }
});
test("late Identity fetch cannot restore a signed-out identity", async () => {
  const identity = deferred<{ id: string } | null>();
  const f = fixture(() => identity.promise);
  try {
    const first = f.runtime.resolve();
    await f.runtime.replace(null);
    identity.resolve({ id: "old" });
    expect((await first).phase).toBe("anonymous");
    expect(f.sockets).toHaveLength(0);
  } finally { f.runtime.close(); }
});
test("identity replacement fences a late binding and disposal releases it", async () => {
  const binding = deferred<Binding>(); let freed = 0;
  const f = fixture(async () => ({ id: "alice" }), () => binding.promise);
  const first = f.runtime.refresh();
  await Promise.resolve(); await Promise.resolve();
  await f.runtime.replace(null);
  binding.resolve({ connect: () => "", invoke: () => "", receive: () => "", free() { freed++; } });
  await first;
  expect(f.runtime.getSnapshot().phase).toBe("anonymous");
  expect(freed).toBe(1); expect(f.sockets).toHaveLength(0);
  f.runtime.close();
});
test("physical reconnect retains ready pages; confirmed expiry clears documents", async () => {
  let account: { id: string } | null = { id: "alice" };
  const f = fixture(async () => account);
  try {
    await f.runtime.refresh();
    f.sockets[0]!.receive({ Attached: { resumed: false } }); f.sockets[0]!.receive({ manifest: true });
    const epoch = f.runtime.getSnapshot().epoch;
    const publication = f.publications.at(-1);
    f.sockets[0]!.receive({ reset: true });
    expect(f.publications.at(-1)).toBe(publication);
    f.sockets[0]!.close();
    expect(f.runtime.getSnapshot().phase).toBe("ready");
    await f.runtime.refresh();
    expect(f.created).toBe(1); expect(f.runtime.getSnapshot().epoch).toBe(epoch);
    account = null; await f.runtime.refresh();
    expect(f.runtime.getSnapshot().phase).toBe("anonymous");
    expect(f.publications.at(-1)).toBeNull(); expect(f.freed).toBe(1);
  } finally { f.runtime.close(); }
});
test("disposal settles route readiness even while Identity IO is pending", async () => {
  const identity = deferred<{ id: string } | null>();
  const f = fixture(() => identity.promise);
  const route = f.runtime.resolve();
  f.runtime.close();
  await expect(route).rejects.toThrow("Client closed");
  identity.resolve({ id: "alice" });
  await Promise.resolve(); await Promise.resolve();
  expect(f.sockets).toHaveLength(0);
});
