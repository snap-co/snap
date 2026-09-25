import { test, expect, type Page } from "@playwright/test";
import { chattyServer } from "../adapters/chatty";

async function signIn(page: Page, base: string, email: string) {
  await page.goto(base);
  await page.getByRole("link", { name: "Continue with Authy" }).click();
  await page.getByRole("button", { name: "Create an account" }).click();
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill("browser chatty password");
  await page.getByRole("button", { name: "Create account", exact: true }).click();
  await page.getByRole("button", { name: "Continue to Chatty" }).click();
  await page.getByRole("textbox", { name: "Message Chatty" }).waitFor();
}

/** Delays delivery after the real server committed, without delaying other reads. */
async function holdResponse(page: Page, path: string) {
  let arrive!: () => void, release!: () => void;
  const arrived = new Promise<void>(done => { arrive = done; });
  const released = new Promise<void>(done => { release = done; });
  const pattern = `**${path}`;
  const handler = async (route: import("@playwright/test").Route) => { const response = await route.fetch(); arrive(); await released; await route.fulfill({ response }); };
  await page.route(pattern, handler);
  return { arrived, release, remove: () => page.unroute(pattern, handler) };
}

for (const host of ["native", "workers"] as const) {
  test(`Chatty ${host}: navigation fences late send, rename and effort responses`, async ({ page }) => {
    test.setTimeout(120_000);
    const server = await chattyServer({ host });
    const held: Awaited<ReturnType<typeof holdResponse>>[] = [];
    try {
      await signIn(page, server.baseUrl, "navigation@chatty.test");
      const session = await (await page.request.get(`${server.baseUrl}/api/session`)).json();
      const headers = { origin: server.baseUrl, "x-chatty-csrf": session.csrf };
      const create = async (title: string, effort: string) => (await (await page.request.post(`${server.baseUrl}/api/thread/create`, { headers, data: { title, effort } })).json());
      const a = await create("Browser A", "low"), b = await create("Browser B", "high");
      const view = async (id: string) => (await page.request.get(`${server.baseUrl}/api/thread?id=${id}`)).json();
      await page.goto(`${server.baseUrl}/?thread=${a.id}`);
      await expect(page.getByLabel("Thinking effort")).toHaveValue("low");
      await page.locator("nav").getByRole("button", { name: "Browser A", exact: true }).click();
      await expect(page.locator("main header").getByText("Browser A", { exact: true })).toBeVisible();
      const pendingSend = await holdResponse(page, "/api/send"); held.push(pendingSend);
      await page.getByRole("textbox", { name: "Message Chatty" }).fill("A delayed message");
      await page.getByRole("button", { name: "Send message" }).click();
      await pendingSend.arrived;
      await page.locator("nav").getByRole("button", { name: "Browser B", exact: true }).click();
      await expect(page.locator("main header").getByText("Browser B", { exact: true })).toBeVisible();
      pendingSend.release();
      await expect(page.getByLabel("Thinking effort")).toBeEnabled();
      await expect(page.getByLabel("Thinking effort")).toHaveValue("high");
      await expect(page.locator("main header").getByText("Browser B", { exact: true })).toBeVisible();
      await expect(page.getByRole("article").getByText("A delayed message", { exact: true })).toHaveCount(0);
      await pendingSend.remove();
      await page.getByLabel("Thinking effort").selectOption("minimal");
      await expect.poll(async () => (await view(b.id)).thread.effort).toBe("minimal");
      expect((await view(a.id)).thread.effort).toBe("low");

      for (const kind of ["effort", "rename"] as const) {
        await expect(page.getByLabel("Thinking effort")).toBeEnabled();
        const pending = await holdResponse(page, "/api/thread/rename"); held.push(pending);
        if (kind === "effort") await page.getByLabel("Thinking effort").selectOption("high");
        else { page.once("dialog", dialog => void dialog.accept("Browser B renamed")); await page.getByRole("button", { name: "Rename", exact: true }).click(); }
        await pending.arrived;
        await page.locator("nav").getByRole("button", { name: "Browser A", exact: true }).click();
        await expect(page.locator("main header").getByText("Browser A", { exact: true })).toBeVisible();
        pending.release();
        await expect(page.getByLabel("Thinking effort")).toBeEnabled();
        await expect(page.getByLabel("Thinking effort")).toHaveValue("low");
        await expect(page.locator("main header").getByText("Browser A", { exact: true })).toBeVisible();
        expect((await view(a.id)).thread).toMatchObject({ title: "Browser A", effort: "low" });
        await pending.remove();
        const title = kind === "rename" ? "Browser B renamed" : "Browser B";
        await page.locator("nav").getByRole("button", { name: title, exact: true }).click();
        await expect(page.locator("main header").getByText(title, { exact: true })).toBeVisible();
      }
      expect((await view(b.id)).thread).toMatchObject({ title: "Browser B renamed", effort: "high" });
    } finally { for (const pending of held) pending.release(); await server.close(); }
  });
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
