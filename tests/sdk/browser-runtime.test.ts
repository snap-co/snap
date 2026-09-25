// SDK contracts exercised through bindings, without React or a DOM.
import { test, expect } from "bun:test";
import { createClient, startHealthy } from "../fixtures/healthy/client";
import { deadline } from "../adapters/server";

const wasm = await Bun.file(
  new URL(
    "../fixtures/healthy/.snap/bindings/healthy_wasm_bg.wasm",
    import.meta.url,
  ),
).arrayBuffer();

test("Rust browser SDK preserves errors, checks correlation, and supports concurrent queries", async () => {
  let mode = "wrong";
  const peer = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      if (mode === "malformed") return new Response("not JSON");
      return Response.json({
        key: "transport.complete",
        target:
          mode === "wrong"
            ? "wrong"
            : request.headers.get("x-snap-operation-id"),
        payload:
          mode === "error"
            ? {
                ok: false,
                error: {
                  _tag: "UnavailableError",
                  message: "Service unavailable",
                },
              }
            : { ok: true, payload: { status: "OK" } },
      });
    },
  });
  const client = await createClient({
    baseUrl: peer.url.href,
    build: "test",
    wasm,
  });
  try {
    await expect(client.health.up()).rejects.toMatchObject({
      _tag: "ContractViolationError",
    });
    mode = "malformed";
    await expect(client.health.up()).rejects.toMatchObject({
      _tag: "ContractViolationError",
    });
    mode = "error";
    await expect(client.health.up()).rejects.toMatchObject({
      _tag: "UnavailableError",
      message: "Service unavailable",
    });
    mode = "ok";
    expect(await Promise.all([client.health.up(), client.health.up()])).toEqual(
      [{ status: "OK" }, { status: "OK" }],
    );
  } finally {
    await client.close();
    await peer.stop(true);
  }
});

test("closing cancels outstanding browser IO and rejects new work", async () => {
  const arrived = Promise.withResolvers<void>();
  const release = Promise.withResolvers<Response>();
  const peer = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch() {
      arrived.resolve();
      return release.promise;
    },
  });
  const client = await createClient({
    baseUrl: peer.url.href,
    build: "test",
    wasm,
  });
  try {
    const result = client.health.up().catch((error: unknown) => error);
    await deadline(arrived.promise, 2_000);
    await deadline(client.close(), 2_000);
    expect(await result).toMatchObject({ _tag: "UnavailableError" });
    await expect(client.health.up()).rejects.toMatchObject({
      _tag: "UnavailableError",
    });
    await client.close();
  } finally {
    release.resolve(new Response("closed"));
    await client.close();
    await peer.stop(true);
  }
});

test("resident SDK exposes stable immutable observations and closes subscriptions", async () => {
  const peer = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      return Response.json({
        key: "transport.complete",
        target: request.headers.get("x-snap-operation-id"),
        payload: { ok: true, payload: { status: "OK" } },
      });
    },
  });
  const client = await startHealthy({
    baseUrl: peer.url.href,
    build: "test",
    wasm,
  });
  try {
    const observed = Promise.withResolvers<void>();
    const unsubscribe = client.subscribe(() => {
      if (client.getSnapshot().status === "ok") observed.resolve();
    });
    if (client.getSnapshot().status === "ok") observed.resolve();
    await deadline(observed.promise, 2_000);
    const snapshot = client.getSnapshot();
    expect(client.getSnapshot()).toBe(snapshot);
    expect(snapshot.samples.at(-1)?.ok).toBe(true);
    expect(Object.isFrozen(snapshot.samples)).toBe(true);
    unsubscribe();
    await deadline(client.close(), 2_000);
    expect(client.getSnapshot()).toBe(snapshot);
    expect(() => client.subscribe(() => {})).toThrow("closed");
  } finally {
    await client.close();
    await peer.stop(true);
  }
});
