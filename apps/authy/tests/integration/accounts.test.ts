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
    expect((await post("/api/signup", body, "", "https://other.invalid")).status).toBe(403);
    expect((await post("/api/signup", { ...body, password: "short" })).status).toBe(400);
    const created = await post("/api/signup", body);
    expect(created.status).toBe(200);
    const first = created.headers.get("set-cookie")!.split(";")[0];
    const account = (await created.json()).account;
    expect(account.email).toBe("account@example.test");
    expect((await post("/api/signup", body)).status).toBe(409);
    expect((await post("/api/login", { ...body, password: "incorrect password" })).status).toBe(401);
    const loggedIn = await post("/api/login", body);
    const second = loggedIn.headers.get("set-cookie")!.split(";")[0];
    expect((await loggedIn.json()).account.identity).toBe(account.identity);
    const sessions = (await invoke(server.base, first, "authy.sessions", null)).sessions;
    expect(sessions).toHaveLength(2);
    expect(sessions.filter((s: { current: boolean }) => s.current)).toHaveLength(1);
    const credentials = (await invoke(server.base, first, "authy.credentials", null)).credentials;
    expect(credentials).toEqual([{ label: "account@example.test", kind: "password", removable: false }]);
    expect((await post("/api/logout", { scope: "all" }, first)).status).not.toBe(200);
    await invoke(server.base, first, "authy.logout", { scope: "others" });
    expect((await (await request("/api/session", second)).json()).account).toBeNull();
    await server.restart();
    expect((await (await request("/api/session", first)).json()).account.identity).toBe(account.identity);
    await invoke(server.base, first, "authy.logout", { scope: "current" });
    expect((await (await request("/api/session", first)).json()).account).toBeNull();
  } finally { await server.close(); }
}, 30000);

test("canonical HTTPS origin selects secure host cookie and survives restart", async () => {
  const server = await host(undefined, { SNAP_ORIGIN: "HTTPS://AUTHY.EXAMPLE:443" });
  try {
    const discovery = await (await fetch(`${server.base}/.well-known/openid-configuration`)).json();
    expect(discovery.issuer).toBe("https://authy.example");
    const created = await fetch(`${server.base}/api/signup`, { method: "POST",
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
    const session = await (await fetch(`${server.base}/api/session`, { headers: { cookie } })).json();
    expect(session.account.email).toBe("secure@example.test");
  } finally { await server.close(); }
}, 30000);
