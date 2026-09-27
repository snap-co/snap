import { test, expect } from "bun:test";
import { authyServer } from "../support/authy";

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
  } finally { await server.close(); }
}, 30_000); // Real host startup includes random RSA key provisioning.
