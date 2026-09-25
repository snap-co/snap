import { test, expect } from "@playwright/test";
import { chattyServer } from "../adapters/chatty";

for (const host of ["native", "workers"] as const) {
  test(`Chatty ${host}: register at Authy, consent, chat, reload and sign out`, async ({ page }) => {
    test.setTimeout(120_000);
    const server = await chattyServer({ host });
    const passwords: string[] = [];
    page.on("request", request => { if (request.url().startsWith(server.baseUrl) && request.postData()?.includes("browser chatty password")) passwords.push(request.url()); });
    try {
      await page.goto(server.baseUrl);
      await page.getByRole("link", { name: "Continue with Authy" }).click();
      await expect(page).toHaveURL(new RegExp(`^${server.authy.replaceAll(".", "\\.")}/`));
      await page.getByRole("button", { name: "Create an account" }).click();
      await page.getByLabel("Email").fill("browser@chatty.test");
      await page.getByLabel("Password").fill("browser chatty password");
      await page.getByRole("button", { name: "Create account", exact: true }).click();
      await page.getByRole("button", { name: "Continue to Chatty" }).click();
      await expect(page.getByRole("textbox", { name: "Message Chatty" })).toBeVisible();
      await page.getByRole("textbox", { name: "Message Chatty" }).fill("browser question");
      await page.getByRole("button", { name: "Send message" }).click();
      await expect(page.getByText("Reply to browser question", { exact: true })).toBeVisible();
      await page.reload();
      await expect(page.getByText("Reply to browser question", { exact: true })).toBeVisible();
      await page.getByText("Reasoning summary", { exact: true }).click();
      await expect(page.getByText("A short supplied summary.")).toBeVisible();
      await page.getByRole("button", { name: "Sign out", exact: true }).click();
      await page.getByRole("button", { name: "Sign out", exact: true }).click();
      await expect(page.getByRole("link", { name: "Continue with Authy" })).toBeVisible();
      expect(passwords).toEqual([]);
    } finally { await server.close(); }
  });
}
