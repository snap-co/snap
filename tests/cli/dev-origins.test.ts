import { expect, test } from "bun:test";
import { chromium } from "@playwright/test";
import { request } from "node:http";
import { pair } from "../../apps/chatty/tests/support/pair";
import { devHosts, originsFor } from "../../scripts/dev-network";

test("network dev preserves alias callbacks and logout, blocks spoofed hosts and origins, and serves HMR", async () => {
  const server = await pair({ dev: true });
  const browser = await chromium.launch();
  try {
    const aliases = originsFor(await devHosts(), new URL(server.base).port);
    const candidates = aliases.filter(o => o !== server.base);
    expect(candidates.length).toBeGreaterThan(0);
    for (const origin of aliases) {
      const login = await fetch(`${origin}/auth/login`, { redirect: "manual", headers: { "x-snap-dev-origin": "http://evil.test" } });
      expect(login.status).toBe(303);
      const target = new URL(login.headers.get("location")!);
      expect(target.origin).toBe(server.authy.base);
      expect(target.searchParams.get("redirect_uri")).toBe(`${origin}/auth/callback`);
      expect((await fetch(`${origin}/@vite/client`)).ok).toBe(true);
    }
    for (const headers of [
      { host: "evil.test", origin: "http://evil.test" },
      { host: new URL(server.base).host, origin: "http://evil.test" },
      { host: new URL(server.base).host, origin: "null" },
    ]) {
      expect((await fetch(`${server.base}/api/session`, { headers })).status).toBe(403);
      const rejected = await new Promise<number>((resolve, reject) => {
        const req = request(`${server.base}/transport`, { headers: { ...headers, connection: "Upgrade", upgrade: "websocket", "sec-websocket-version": "13", "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==" } }, res => { res.resume(); resolve(res.statusCode!); });
        req.on("upgrade", (_res, socket) => { socket.destroy(); reject(new Error("Spoofed WebSocket accepted")); });
        req.on("error", reject); req.setTimeout(5000, () => req.destroy(new Error("WebSocket rejection stalled"))); req.end();
      });
      expect(rejected).toBe(403);
    }
    // A separate browser session on each authority exercises host-only cookies,
    // code exchange, authenticated WS mutations and RP-initiated logout.
    for (const origin of aliases) {
      const context = await browser.newContext();
      try {
        const page = await context.newPage();
        const hmr = page.waitForEvent("websocket", { predicate: ws => ws.url().includes("token=") });
        await page.goto(origin);
        expect(new URL((await hmr).url()).host).toBe(new URL(origin).host);
        await page.getByRole("link", { name: /Continue with Authy/ }).click();
        await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
        await page.getByLabel("Email", { exact: true }).fill(`network-${crypto.randomUUID()}@example.test`);
        await page.getByLabel("Password", { exact: true }).fill("Network development password");
        await page.getByRole("button", { name: "Create account", exact: true }).click();
        await page.getByText(origin, { exact: true }).waitFor();
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        await page.getByLabel("Message Chatty").fill("Alias WebSocket works");
        await page.getByRole("button", { name: "Send message", exact: true }).click();
        await page.locator(".transcript .user-message p").filter({ hasText: "Alias WebSocket works" }).waitFor({ timeout: 10000 });
        expect(new URL(page.url()).origin).toBe(origin);
        await page.getByRole("button", { name: /Sign out/ }).click();
        await page.getByRole("button", { name: "Confirm sign out", exact: true }).click();
        await page.getByRole("link", { name: /Continue with Authy/ }).waitFor();
        expect(new URL(page.url()).origin).toBe(origin);
      } finally { await context.close(); }
    }
  } catch (error) { console.error(server.logs); throw error; }
  finally { await browser.close(); await server.close(); }
}, 120000);
