import { test, expect } from "@playwright/test";

test("account, optimistic Document profile, reload, logout and login", async ({ page, context, baseURL }) => {
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  const email = `profile-${Date.now()}@example.test`;
  await page.goto("/");
  await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill("a test password for Authy");
  await page.getByRole("button", { name: "Create account", exact: true }).click();
  await expect(page.getByLabel("Name", { exact: true })).toBeVisible();
  const cookies = await context.cookies();
  const session = cookies.find(cookie => cookie.name === "authy_session")!;
  expect(session.httpOnly).toBe(true);
  expect(session.sameSite).toBe("Lax");
  await page.getByLabel("Name", { exact: true }).fill("Document Person");
  await page.getByLabel("Bio", { exact: true }).fill("Persistent, private profile");
  await page.getByRole("button", { name: "Save profile", exact: true }).click();
  await expect(page.getByText("Saved revision 2", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Save profile", exact: true })).toBeEnabled();
  await page.reload();
  await expect(page.getByLabel("Name", { exact: true })).toHaveValue("Document Person");
  await expect(page.getByLabel("Bio", { exact: true })).toHaveValue("Persistent, private profile");
  const second = await context.newPage();
  await second.goto(baseURL!);
  await expect(second.getByLabel("Name", { exact: true })).toHaveValue("Document Person");
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await expect(page.getByLabel("Email", { exact: true })).toBeVisible();
  await expect(second.getByLabel("Name", { exact: true })).not.toBeVisible();
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill("a test password for Authy");
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByLabel("Name", { exact: true })).toHaveValue("Document Person");
  expect(errors).toEqual([]);
  await second.close();
});

test("OAuth returns through password login and explicit consent; forced login asks again", async ({ page, baseURL }) => {
  const email = `oauth-browser-${Date.now()}@example.test`;
  const password = "browser OIDC password";
  const parameters = new URLSearchParams({ client_id: "chatty", redirect_uri: `${process.env.AUTHY_TEST_RP}/auth/callback`,
    response_type: "code", scope: "openid profile email", state: "browser-state", nonce: "browser-nonce",
    code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", code_challenge_method: "S256" });
  await page.goto(`${baseURL}/oauth/authorize?${parameters}`);
  await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill(password);
  await page.getByRole("button", { name: "Create account", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Authorize application" })).toBeVisible();
  await expect(page.getByText(/Chatty is requesting access/)).toBeVisible();
  await expect(page.getByText("See your email address", { exact: true })).toBeVisible();
  const consentResponse = page.waitForResponse(response => response.url().endsWith("/oauth/authorize") && response.request().method() === "POST");
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  const consent = await consentResponse;
  expect(consent.status(), `Origin: ${await consent.request().headerValue("origin")}`).toBe(303);
  await expect(page).toHaveURL(/\/auth\/callback\?.*code=/);
  expect(new URL(page.url()).searchParams.get("state")).toBe("browser-state");
  parameters.set("prompt", "login");
  await page.goto(`${baseURL}/oauth/authorize?${parameters}`);
  await expect(page.getByLabel("Password", { exact: true })).toBeVisible();
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill(password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await expect(page).toHaveURL(/error=access_denied/);
  const logout = new URLSearchParams({ client_id: "chatty", post_logout_redirect_uri: `${process.env.AUTHY_TEST_RP}/auth/logged-out`, state: "logout-state" });
  await page.goto(`${baseURL}/oauth/logout?${logout}`);
  await expect(page.getByRole("heading", { name: "Sign out of Authy" })).toBeVisible();
  await page.getByRole("button", { name: "Confirm sign out", exact: true }).click();
  await expect(page).toHaveURL(/\/auth\/logged-out\?state=logout-state/);
  await page.goto(baseURL!);
  await expect(page.getByLabel("Email", { exact: true })).toBeVisible();
});

test("auth protocol pages share sign-in styling and work without JavaScript", async ({ browser, baseURL }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 390, height: 844 } });
  try {
    const enrolled = await context.request.post(`${baseURL}/api/signup`, { headers: { origin: baseURL! }, data: { email: `static-auth-${Date.now()}@example.test`, password: "static auth fixture password" } });
    expect(enrolled.ok()).toBe(true);
    const page = await context.newPage();
    const parameters = new URLSearchParams({ client_id: "chatty", redirect_uri: `${process.env.AUTHY_TEST_RP}/auth/callback`, response_type: "code", scope: "openid profile email", state: "static-state", nonce: "static-nonce", prompt: "consent", code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", code_challenge_method: "S256" });
    const response = await page.goto(`${baseURL}/oauth/authorize?${parameters}`);
    expect(response?.headers()["content-security-policy"]).toContain("default-src 'none'");
    await expect(page.getByRole("link", { name: "Authy home" })).toBeVisible();
    await expect(page.getByText("Sign you in", { exact: true })).toBeVisible();
    await expect(page.getByText("Read your profile", { exact: true })).toBeVisible();
    await expect(page.getByText(process.env.AUTHY_TEST_RP!, { exact: true })).toBeVisible();
    expect(await page.locator(".auth-shell").evaluate(element => getComputedStyle(element).backgroundColor)).toBe("rgb(25, 30, 39)");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: "/tmp/opencode/authy-consent-mobile.png", fullPage: true });
    await page.setViewportSize({ width: 1100, height: 900 });
    await page.screenshot({ path: "/tmp/opencode/authy-consent-desktop.png", fullPage: true });
    await page.getByRole("button", { name: "Deny", exact: true }).click();
    await expect(page).toHaveURL(/error=access_denied/);
    const logout = new URLSearchParams({ client_id: "chatty", post_logout_redirect_uri: `${process.env.AUTHY_TEST_RP}/auth/logged-out`, state: "static-logout" });
    await page.goto(`${baseURL}/oauth/logout?${logout}`);
    await expect(page.getByRole("heading", { name: "Sign out of Authy" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Stay signed in" })).toBeVisible();
    await page.getByRole("button", { name: "Confirm sign out" }).click();
    await expect(page).toHaveURL(/state=static-logout/);
    const invalid = await page.goto(`${baseURL}/oauth/resume?request=expired`);
    expect(invalid?.status()).toBe(400);
    await expect(page.getByRole("heading", { name: "We couldn't complete this request" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Return to Authy" })).toBeVisible();
    const machine = await context.request.get(`${baseURL}/oauth/resume?request=expired`, { headers: { accept: "application/json" } });
    expect(machine.headers()["content-type"]).toContain("application/json");
  } finally { await context.close(); }
});
