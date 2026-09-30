import { expect, test } from "bun:test";
import { BrowserRuntime, type Binding, type Publication } from "./runtime";

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; }
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
function fixture(fetch: () => Promise<{ id: string } | null>, create?: () => Promise<Binding>) {
  const sockets: Socket[] = [], publications: (Publication | null)[] = [];
  let created = 0, freed = 0, sequence = 0;
  const runtime = new BrowserRuntime({
    identity: { fetch }, key: account => account.id,
    create: async () => { created++; return create ? create() : {
      connect: () => "", invoke: (operation, input) => JSON.stringify({ Invoke: { id: ++sequence, operation, input: JSON.parse(input) } }), free: () => { freed++; },
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
    const call = f.runtime.invoke("example", null);
    const rejection = call.then(() => { throw new Error("Expected ended session"); }, (error: Error) => error);
    f.sockets[0]!.receive({ Events: [{ Accepted: { id: 1 } }] });
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
    expect((await rejection).message).toContain("Account session ended");
    expect(f.sockets[1]!.sent).toHaveLength(0);
  } finally { f.runtime.close(); }
});
test("reconnect retries a failed Identity check and recovers the accepted call with the same ID", async () => {
  const unavailable = deferred<void>(), recovered = deferred<void>();
  let checks = 0;
  const f = fixture(async () => {
    checks++;
    if (checks === 2) { unavailable.resolve(); throw new Error("backend restarting"); }
    if (checks === 3) recovered.resolve();
    return { id: "alice" };
  });
  try {
    await f.runtime.refresh();
    f.sockets[0]!.receive({ Attached: { resumed: false } }); f.sockets[0]!.receive({ manifest: true });
    const call = f.runtime.invoke<string>("example.change", { value: "new" });
    // Observe either settlement immediately, including cleanup on assertion failure.
    const outcome = call.then(value => ({ value }), error => ({ error }));
    const frame = f.sockets[0]!.sent[0]!;
    const id = JSON.parse(frame).Invoke.id;
    f.sockets[0]!.receive({ Events: [{ Accepted: { id } }] });
    f.sockets[0]!.close();
    await unavailable.promise;
    expect(f.sockets).toHaveLength(1);
    await recovered.promise;
    await f.runtime.refresh();
    expect(checks).toBe(3); expect(f.created).toBe(1);
    f.sockets[1]!.receive({ Attached: { resumed: true } });
    f.sockets[1]!.receive({ manifest: true });
    expect(f.sockets[0]!.sent).toEqual([frame]);
    expect(f.sockets[1]!.sent).toEqual([frame]);
    f.sockets[1]!.receive({ Events: [{ Completed: { id, outcome: { Ok: "recovered" } } }] });
    expect(await outcome).toEqual({ value: "recovered" });
    expect(f.runtime.getSnapshot().connection).toBe("connected");
    expect(f.runtime.getSnapshot().error).toBeNull();
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
for (const terminal of [{ Failed: "StaleConnection" }, { Failed: "InvalidBearer" }, "Detached"]) {
  test(`terminal ${JSON.stringify(terminal)} on reconnect settles an accepted call without replay`, async () => {
    const f = fixture(async () => ({ id: "alice" }));
    try {
      await f.runtime.refresh();
      f.sockets[0]!.receive({ Attached: { resumed: false } }); f.sockets[0]!.receive({ manifest: true });
      let outcome = "pending";
      const call = f.runtime.invoke("example", null).then(() => { outcome = "resolved"; }, () => { outcome = "rejected"; });
      f.sockets[0]!.receive({ Events: [{ Accepted: { id: 1 } }] });
      f.sockets[0]!.close();
      await f.runtime.refresh();
      f.sockets[1]!.receive(terminal);
      await Promise.resolve(); await Promise.resolve();
      expect(outcome).toBe("rejected");
      expect(f.sockets[0]!.sent).toHaveLength(1);
      expect(f.sockets[1]!.sent).toHaveLength(0);
      await call;
    } finally { f.runtime.close(); }
  });
}
