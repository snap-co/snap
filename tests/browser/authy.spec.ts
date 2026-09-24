import { test, expect } from "@playwright/test";
import { authyServer } from "../adapters/authy";

for (const dev of [false, true]) {
  test(`Authy ${dev ? "development proxy" : "package"}: sign in, reload, reconnect and remote revocation`, async ({ browser }) => {
    test.setTimeout(120_000);
    const server = await authyServer(dev);
    const first = await browser.newContext();
    const second = await browser.newContext();
    const page = await first.newPage();
    const other = await second.newPage();
    const errors: string[] = [];
    page.on("pageerror", e => errors.push(e.message)); other.on("pageerror", e => errors.push(e.message));
    try {
      await page.goto(server.baseUrl);
      await page.getByRole("button", { name: "Create an account", exact: true }).click();
      await page.getByLabel("Email", { exact: true }).fill("person@example.test");
      await page.getByLabel("Password", { exact: true }).fill("correct horse battery");
      await page.getByRole("button", { name: "Create account", exact: true }).click();
      await expect(page.getByRole("heading", { name: "You're signed in" })).toBeVisible();
      await expect(page.getByTestId("connection")).toHaveText("connected");
      await expect(page.getByText("person@example.test", { exact: true })).toBeVisible();
      const identity = await page.getByTestId("identity").textContent();
      const cookie = (await first.cookies()).find(c => c.name === "authy_session")!;
      expect(cookie.httpOnly).toBe(true); expect(cookie.sameSite).toBe("Lax"); expect(cookie.expires).toBeGreaterThan(Date.now() / 1000);
      await page.reload();
      await expect(page.getByTestId("identity")).toHaveText(identity!);
      await expect(page.getByTestId("connection")).toHaveText("connected");
      await other.goto(server.baseUrl);
      await other.getByLabel("Email", { exact: true }).fill("PERSON@example.test");
      await other.getByLabel("Password", { exact: true }).fill("wrong password");
      await other.getByRole("button", { name: "Sign in", exact: true }).click();
      await expect(other.getByRole("alert")).toContainText("Invalid credential");
      await other.getByLabel("Password", { exact: true }).fill("correct horse battery");
      await other.getByRole("button", { name: "Sign in", exact: true }).click();
      await expect(other.getByTestId("identity")).toHaveText(identity!);
      await expect(other.getByTestId("connection")).toHaveText("connected");
      await expect(other.getByText("Another session", { exact: true })).toBeVisible();
      if (!dev) {
        await server.restart(async () => {
          await expect(page.getByTestId("connection")).not.toHaveText("connected");
        });
        await expect(page.getByTestId("connection")).toHaveText("connected", { timeout: 15_000 });
        await page.reload();
        await expect(page.getByTestId("identity")).toHaveText(identity!);
        await expect(other.getByTestId("connection")).toHaveText("connected");
      }
      await other.getByRole("button", { name: "Sign out other sessions" }).click();
      await expect(page.getByRole("heading", { name: "Sign in", exact: true })).toBeVisible();
      await expect(other.getByTestId("identity")).toHaveText(identity!);
      await other.getByRole("button", { name: "Sign out", exact: true }).click();
      await expect(other.getByRole("heading", { name: "Sign in", exact: true })).toBeVisible();
      await other.reload();
      await expect(other.getByRole("heading", { name: "Sign in", exact: true })).toBeVisible();
      expect(errors).toEqual([]);
    } finally { try { await first.close(); await second.close(); } finally { await server.close(); } }
  });
}
