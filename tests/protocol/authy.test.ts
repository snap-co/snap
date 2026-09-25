import { test, expect } from "bun:test";
import { authyServer } from "../adapters/authy";
import { deadline } from "../adapters/server";
import { workersServer } from "../adapters/workers";

function socket(base: string, cookie?: string, build = "healthy-smoke") {
  const url = new URL("/_transport/ws", base);
  url.protocol = "ws:"; url.searchParams.set("build", build); url.searchParams.set("clientId", crypto.randomUUID());
  const ws = new WebSocket(url, { headers: { Origin: base, ...(cookie ? { Cookie: cookie } : {}) } });
  const messages: any[] = [];
  const waiters: ((value: any) => void)[] = [];
  ws.addEventListener("message", event => { const value = JSON.parse(String(event.data)); const waiter = waiters.shift(); if (waiter) waiter(value); else messages.push(value); });
  const closed = new Promise<CloseEvent>(resolve => ws.addEventListener("close", resolve, { once: true }));
  return { ws, closed, next: () => deadline(messages.length ? Promise.resolve(messages.shift()) : new Promise(resolve => waiters.push(resolve)), 5_000) };
}

for (const host of ["native", "workers"] as const) {
test(`${host} password/session wire: cookies, identity modes, Message admission and persistent authority`, async () => {
  const server = await (host === "native" ? authyServer() : workersServer("authy"));
  const sockets: ReturnType<typeof socket>[] = [];
  const request = async (key: string, payload?: unknown, cookie?: string, extra: Record<string,string> = {}) => {
    const response = await fetch(`${server.baseUrl}/${key.replaceAll(".", "/")}`, { method: key === "identity.fetch" ? "GET" : "POST", headers: { "x-snap-build": "healthy-smoke", "x-snap-operation-id": "wire-operation", "content-type": "application/json", ...(cookie ? { cookie } : {}), ...extra }, body: payload === undefined ? undefined : JSON.stringify(payload) });
    return { response, body: await response.json() };
  };
  try {
    const denied = await request("identity.fetch", undefined, undefined, { "x-snap-build": "wrong" });
    expect(denied.response.status).toBe(409);
    const short = await request("account.create", { email: "test@example.test", password: "short" });
    expect(short.body.payload.error._tag).toBe("InvalidInputError");
    const created = await request("account.create", { email: "test@example.test", password: "long password" });
    expect(created.body).toEqual({ key: "transport.complete", target: "wire-operation", payload: { ok: true, sessionChanged: true } });
    const setCookie = created.response.headers.get("set-cookie")!;
    expect(setCookie).toContain("Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000");
    const cookie = setCookie.split(";")[0];
    expect(cookie).toMatch(/^authy_session=[\w-]+\.[\w-]+$/);
    expect(created.response.headers.get("cache-control")).toBe("private, no-store");
    const identity = (await request("identity.fetch", undefined, cookie)).body.payload.payload.identityId;
    expect(identity).toMatch(/^[0-9a-f-]{36}$/);
    const forbidden = await request("identity.password.acquire", { kind: "user", email: "test@example.test", password: "long password" }, cookie);
    expect(forbidden.response.status).toBe(403);
    expect(forbidden.body.payload.error._tag).toBe("IdentityForbiddenError");
    const wrong = await request("identity.password.acquire", { kind: "user", email: "test@example.test", password: "wrong" });
    expect(wrong.body.payload.error).toEqual({ _tag: "OperationError", failure: { _tag: "InvalidCredentialError", message: "Invalid credential" } });
    const duplicateCookie = await request("identity.fetch", undefined, `${cookie}; ${cookie}`);
    expect(duplicateCookie.response.status).toBe(400);
    expect(duplicateCookie.body.payload.error._tag).toBe("InvalidInputError");
    expect((await request("identity.fetch", undefined, "authy_session=forged.bad")).body.payload.payload.identityId).toBeNull();
    expect((await request("identity.release", { scope: "all" }, cookie, { origin: "https://other.example" })).response.status).toBe(403);
    const anonymous = socket(server.baseUrl); sockets.push(anonymous);
    expect((await deadline(anonymous.closed, 5_000)).code).toBe(4001);
    const stale = socket(server.baseUrl, cookie, "old-build"); sockets.push(stale);
    const close = await deadline(stale.closed, 5_000);
    expect(close.code).toBe(4003); expect(close.reason).toBe("healthy-smoke");
    const live = socket(server.baseUrl, cookie); sockets.push(live);
    const epoch = await live.next();
    expect(epoch.key).toBe("transport.epoch");
    const prefix = epoch.payload.epoch;
    live.ws.send(JSON.stringify({ operationId: `${prefix}:1`, key: "identity.credentials" }));
    expect(await live.next()).toEqual({ key: "transport.ack", target: `${prefix}:1` });
    const credentials = await live.next();
    expect(credentials.target).toBe(`${prefix}:1`);
    expect(credentials.payload.payload.credentials[0]).toMatchObject({ method: "password", label: "test@example.test", removable: false });
    live.ws.send(JSON.stringify({ operationId: `${prefix}:1`, key: "identity.credentials" }));
    expect((await live.next()).payload.error).toMatchObject({ _tag: "IndeterminateError", admission: "accepted" });
    live.ws.send(JSON.stringify({ operationId: `${prefix}:3`, key: "identity.sessions" }));
    expect((await live.next()).payload.error).toMatchObject({ _tag: "IndeterminateError", admission: "unknown" });
    live.ws.send(JSON.stringify({ operationId: `${prefix}:2`, key: "identity.sessions", payload: {} }));
    expect((await live.next()).payload.error._tag).toBe("InvalidInputError");
    live.ws.send(JSON.stringify({ operationId: `${prefix}:3`, key: "identity.sessions" }));
    expect((await live.next()).key).toBe("transport.ack");
    expect((await live.next()).payload.payload.sessions[0].current).toBe(true);
    live.ws.send(JSON.stringify({ operationId: `${prefix}:4`, key: "identity.sessions", payload: null }));
    expect((await live.next()).key).toBe("transport.ack");
    expect((await live.next()).payload.ok).toBe(true);
    live.ws.close(); await deadline(live.closed, 5_000);
    await server.restart();
    expect((await request("identity.fetch", undefined, cookie)).body.payload.payload.identityId).toBe(identity);
    const released = await request("identity.release", { scope: "current" }, cookie);
    expect(released.body).toEqual({ key: "transport.complete", target: "wire-operation", payload: { ok: true, sessionChanged: true } });
    expect(released.response.headers.get("set-cookie")).toContain("Max-Age=0");
    expect((await request("identity.fetch", undefined, cookie)).body.payload.payload.identityId).toBeNull();
  } finally { for (const { ws } of sockets) ws.close(); await server.close(); }
}, 120_000);
}
