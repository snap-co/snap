import { test, expect } from "bun:test";
import { host } from "../support/upgraded-host";

async function invoke(base: string, cookie: string, operation: string, input: unknown) {
  const socket = new WebSocket(`${base.replace(/^http/, "ws")}/transport`, { headers: { cookie, origin: base } });
  return await new Promise<any>((resolve, reject) => {
    let accepted = false;
    const timer = setTimeout(() => { socket.close(); reject(new Error("Invocation timed out")); }, 5000);
    const finish = (value: unknown, error = false) => { clearTimeout(timer); socket.close(); error ? reject(new Error(JSON.stringify(value))) : resolve(value); };
    socket.onopen = () => socket.send(JSON.stringify({ Connect: { bearer: "", client_id: crypto.randomUUID() } }));
    socket.onerror = () => finish("WebSocket failed", true);
    socket.onmessage = event => {
      const frame = JSON.parse(String(event.data));
      if (frame.Attached) socket.send(JSON.stringify({ Invoke: { id: 1, operation, input } }));
      if (frame.Failed) finish(frame.Failed, true);
      for (const event of frame.Events ?? []) {
        if (event.Accepted) accepted = true;
        if (event.Completed) {
          if (event.Completed.outcome.Err) finish(event.Completed.outcome.Err, true);
          else { expect(accepted).toBe(true); finish(event.Completed.outcome.Ok); }
        }
      }
    };
  });
}

test("WebSocket account operations preserve credential, cookie and session authority", async () => {
  const server = await host();
  const body = { email: "  Account@Example.test ", password: "account fixture password" };
  const request = (path: string, cookie = "") => fetch(`${server.base}${path}`, { headers: { cookie } });
  const post = (path: string, value: unknown, cookie = "", origin = server.base) => fetch(`${server.base}${path}`, {
    method: "POST", headers: { cookie, origin, "content-type": "application/json" }, body: JSON.stringify(value),
  });
  try {
    for (const path of ["/api/signup", "/api/login", "/api/session"]) {
      expect((await post(path, body)).status).toBe(404);
      expect((await request(path)).status).toBe(404);
    }
    const upgrade = (cookie = "") => fetch(`${server.base}/transport`, { headers: { cookie, origin: server.base, connection: "upgrade", upgrade: "websocket", "sec-websocket-version": "13", "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==" } });
    expect((await upgrade()).status).toBe(401);
    expect((await post("/identity/enroll", body, "", "https://other.invalid")).status).toBe(403);
    expect((await post("/identity/enroll", { ...body, password: "short" })).status).toBe(400);
    const created = await post("/identity/enroll", body);
    expect(created.status).toBe(200);
    const first = created.headers.get("set-cookie")!.split(";")[0];
    const completion = await created.json();
    const account = completion.Completed.outcome.Ok.account;
    expect(completion.Completed.outcome.Ok.bearer).toBeUndefined();
    expect(completion.Completed.id).toBe(1);
    expect(account.email).toBe("account@example.test");
    expect((await post("/identity/enroll", body)).status).toBe(409);
    const invalid = await post("/identity/acquire", { ...body, password: "incorrect password" });
    expect(invalid.status).toBe(401);
    expect(invalid.headers.get("set-cookie")).toBeNull();
    const loggedIn = await post("/identity/acquire", body);
    const second = loggedIn.headers.get("set-cookie")!.split(";")[0];
    expect((await loggedIn.json()).Completed.outcome.Ok.account.identity).toBe(account.identity);
    await expect(invoke(server.base, first, "identity.acquire", body)).rejects.toThrow("UnknownOperation");
    expect((await post("/authy/logout", { scope: "all" }, first)).status).not.toBe(200);
    const sessions = (await invoke(server.base, first, "authy.sessions", null)).sessions;
    expect(sessions).toHaveLength(2);
    expect(sessions.filter((s: { current: boolean }) => s.current)).toHaveLength(1);
    const credentials = (await invoke(server.base, first, "authy.credentials", null)).credentials;
    expect(credentials).toEqual([{ label: "account@example.test", kind: "password", removable: false }]);
    expect((await post("/api/logout", { scope: "all" }, first)).status).not.toBe(200);
    await invoke(server.base, first, "authy.logout", { scope: "others" });
    expect((await (await request("/identity/fetch", second)).json()).Completed.outcome.Ok).toBeNull();
    expect((await upgrade(second)).status).toBe(401);
    await server.restart();
    expect((await (await request("/identity/fetch", first)).json()).Completed.outcome.Ok.identity).toBe(account.identity);
    await invoke(server.base, first, "authy.logout", { scope: "current" });
    expect((await (await request("/identity/fetch", first)).json()).Completed.outcome.Ok).toBeNull();
  } finally { await server.close(); }
}, 30000);

test("canonical HTTPS origin selects secure host cookie and survives restart", async () => {
  const server = await host(undefined, { SNAP_ORIGIN: "HTTPS://AUTHY.EXAMPLE:443" });
  try {
    const discovery = await (await fetch(`${server.base}/.well-known/openid-configuration`)).json();
    expect(discovery.issuer).toBe("https://authy.example");
    const created = await fetch(`${server.base}/identity/enroll`, { method: "POST",
      headers: { origin: "https://authy.example", "content-type": "application/json" },
      body: JSON.stringify({ email: "secure@example.test", password: "secure fixture password" }),
    });
    expect(created.status).toBe(200);
    const header = created.headers.get("set-cookie")!;
    expect(header).toStartWith("__Host-authy_session=");
    expect(header).toContain("; Secure");
    expect(header).toContain("; HttpOnly; SameSite=Lax;");
    const cookie = header.split(";")[0];
    await server.restart();
    const session = await (await fetch(`${server.base}/identity/fetch`, { headers: { cookie } })).json();
    expect(session.Completed.outcome.Ok.email).toBe("secure@example.test");
  } finally { await server.close(); }
}, 30000);
