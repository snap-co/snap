import { test, expect } from "bun:test";
import { authyServer } from "../adapters/authy";

test("HTTPS scheme normalization preserves secure cookie naming and session recognition", async () => {
  const server = await authyServer(false, undefined, { origin: "HTTPS://example.test" });
  try {
    const response = await fetch(`${server.baseUrl}/account/create`, {
      method: "POST",
      headers: { "x-snap-build": "healthy-smoke", "content-type": "application/json", origin: "https://example.test" },
      body: JSON.stringify({ email: "secure@example.test", password: "original password" }),
    });
    expect(response.status).toBe(200);
    const cookie = response.headers.get("set-cookie")!;
    expect(cookie).toStartWith("__Host-authy_session=");
    expect(cookie.split("; ")).toContain("Secure");
    const identity = async () => {
      const response = await fetch(`${server.baseUrl}/identity/fetch`, {
        headers: { "x-snap-build": "healthy-smoke", cookie: cookie.split(";")[0] },
      });
      return (await response.json()).payload.payload.identityId;
    };
    const id = await identity();
    expect(typeof id).toBe("string");
    await server.restart();
    expect(await identity()).toBe(id);
  } finally { await server.close(); }
});

// The same public operation sequence must work with either application cache policy.
for (const cached of [false, true]) {
  test(`credential absence before enrollment does not prevent later login, cache=${cached}`, async () => {
    const server = await authyServer(false, undefined, { cached });
    const request = async (key: string, payload: unknown, cookie?: string) => {
      const response = await fetch(`${server.baseUrl}/${key.replaceAll(".", "/")}`, {
        method: "POST",
        headers: { "x-snap-build": "healthy-smoke", "content-type": "application/json", ...(cookie ? { cookie } : {}) },
        body: JSON.stringify(payload),
      });
      return { result: (await response.json()).payload, cookie: response.headers.get("set-cookie")?.split(";")[0] };
    };
    try {
      const credentials = { kind: "user", email: "later@example.test", password: "original password" };
      expect((await request("identity.password.acquire", credentials)).result.error.failure._tag).toBe("InvalidCredentialError");
      const enrolled = await request("account.create", credentials);
      expect(enrolled.result.ok).toBe(true);
      expect((await request("identity.release", { scope: "current" }, enrolled.cookie)).result.ok).toBe(true);
      expect((await request("identity.password.acquire", credentials)).result.payload).toEqual({ _tag: "Approved" });
    } finally { await server.close(); }
  });
}
