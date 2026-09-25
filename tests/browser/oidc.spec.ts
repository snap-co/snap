import { test, expect } from "@playwright/test";
import { oidcBrowserServer, clientSecret } from "../adapters/oidc";

for (const host of ["native", "workers"] as const) {
  test(`Authy ${host}: browser consent reaches the RP and confirmed logout returns`, async ({ page }) => {
    test.setTimeout(120_000);
    const server = await oidcBrowserServer(host);
    const clientOrigin = server.clientOrigin;
    const errors: string[] = [];
    page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
    try {
      // Authy's pages, policies, forms and cross-origin redirects use real servers.
      const created = await page.request.post(`${server.baseUrl}/account/create`, { headers: { "x-snap-build": "healthy-smoke" }, data: { email: "browser-oidc@example.test", password: "browser oidc password" } });
      expect((await created.json()).payload.ok).toBe(true);
      const verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
      const params = new URLSearchParams({ client_id: "chatty", redirect_uri: `${clientOrigin}/auth/callback`, response_type: "code", scope: "openid profile email", state: "browser-state", nonce: "browser-nonce", code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", code_challenge_method: "S256" });
      await page.goto(`${server.baseUrl}/oauth/authorize?${params}`);
      await page.getByRole("button", { name: "Continue to Chatty" }).click();
      await expect(page.getByRole("heading", { name: "Relying party reached" })).toBeVisible();
      const callback = new URL(page.url());
      expect(callback.origin).toBe(clientOrigin); expect(callback.pathname).toBe("/auth/callback");
      expect(callback.searchParams.get("state")).toBe("browser-state");
      const response = await page.request.post(`${server.baseUrl}/oauth/token`, { headers: { authorization: `Basic ${Buffer.from(`chatty:${clientSecret}`).toString("base64")}` }, form: { grant_type: "authorization_code", code: callback.searchParams.get("code")!, redirect_uri: `${clientOrigin}/auth/callback`, code_verifier: verifier } });
      expect(response.status()).toBe(200);
      const tokens = await response.json();
      const logout = new URLSearchParams({ client_id: "chatty", post_logout_redirect_uri: `${clientOrigin}/auth/logged-out`, id_token_hint: tokens.id_token, state: "logout-browser-state" });
      await page.goto(`${server.baseUrl}/oauth/logout?${logout}`);
      await page.getByRole("button", { name: "Sign out", exact: true }).click();
      await expect(page.getByRole("heading", { name: "Relying party reached" })).toBeVisible();
      const destination = new URL(page.url());
      expect(destination.pathname).toBe("/auth/logged-out"); expect(destination.searchParams.get("state")).toBe("logout-browser-state");
      expect((await page.request.get(`${server.baseUrl}/api/account`)).status()).toBe(401);
      expect(errors.filter(error => /Content Security Policy|Origin not allowed/.test(error))).toEqual([]);
    } finally { await server.close(); }
  });
}
