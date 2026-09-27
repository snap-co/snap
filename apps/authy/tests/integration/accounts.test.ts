import { test, expect } from "bun:test";
import { host } from "../support/upgraded-host";

test("HTTP accounts preserve credential, cookie and session authority", async () => {
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
    const sessions = (await (await request("/api/sessions", first)).json()).sessions;
    expect(sessions).toHaveLength(2);
    expect(sessions.filter((s: { current: boolean }) => s.current)).toHaveLength(1);
    const credentials = (await (await request("/api/credentials", first)).json()).credentials;
    expect(credentials).toEqual([{ label: "account@example.test", kind: "password", removable: false }]);
    expect((await post("/api/logout", { scope: "all" }, first, "null")).status).toBe(403);
    expect((await post("/api/logout", { scope: "others" }, first)).status).toBe(200);
    expect((await (await request("/api/session", second)).json()).account).toBeNull();
    await server.restart();
    expect((await (await request("/api/session", first)).json()).account.identity).toBe(account.identity);
    const ended = await post("/api/logout", {}, first);
    expect(ended.headers.get("set-cookie")).toContain("Max-Age=0");
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
