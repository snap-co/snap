import { test, expect, type Page } from "@playwright/test";

async function signup(page: Page, email: string) {
  await page.goto(process.env.CHATTY_TEST_URL!);
  await page.getByRole("link", { name: /Continue with Authy/ }).click();
  await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill("Chatty OAuth fixture password");
  await page.getByRole("button", { name: "Create account", exact: true }).click();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(page.getByLabel("Message Chatty")).toBeVisible();
}
async function send(page: Page, message: string) {
  await page.getByLabel("Message Chatty").fill(message);
  await page.getByRole("button", { name: "Send message", exact: true }).click();
}

test("Authy OAuth, private Document threads, streamed progress, cancellation, file tool, restart and logout", async ({ page, browser }) => {
  const errors: string[] = [];
  const frames: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("websocket", socket => socket.on("framereceived", event => frames.push(String(event.payload))));
  await signup(page, `chatty-${Date.now()}@example.test`);
  await send(page, "Hello");
  await expect(page.getByText("Fixture answer: Hello", { exact: true })).toBeVisible();
  const threadURL = page.url();
  page.once("dialog", dialog => dialog.accept("Renamed conversation"));
  await page.getByRole("button", { name: "Rename", exact: true }).click();
  await expect(page.locator(".topbar strong")).toHaveText("Renamed conversation");
  await page.reload();
  await expect(page.locator(".topbar strong")).toHaveText("Renamed conversation");
  await expect(page.getByText("Fixture answer: Hello", { exact: true })).toBeVisible();
  const heldRequest = page.waitForRequest(request => request.url().endsWith("/api/send") && request.method() === "POST");
  await send(page, "Hold reply");
  const submitted = (await heldRequest).postDataJSON();
  await expect(page.getByText(/^Partial reply\./)).toBeVisible();
  const session = await (await page.request.get(`${process.env.CHATTY_TEST_URL}/api/session`)).json();
  const before = await (await page.request.get(`${process.env.CHATTY_FIXTURE_URL}/stats`)).json();
  const duplicate = await page.request.post(`${process.env.CHATTY_TEST_URL}/api/send`, { headers: { origin: process.env.CHATTY_TEST_URL!, "x-snap-csrf": session.csrf }, data: submitted });
  expect(duplicate.status()).toBe(202);
  const after = await (await page.request.get(`${process.env.CHATTY_FIXTURE_URL}/stats`)).json();
  expect(after.calls).toBe(before.calls);
  await page.getByRole("button", { name: "Stop reply", exact: true }).click();
  await expect(page.getByText("Stopped by you. In-flight remote work may still finish.", { exact: true })).toBeVisible();
  await page.request.get(`${process.env.CHATTY_FIXTURE_URL}/release`);
  await send(page, "Write a note");
  await expect(page.getByText("Saved your note.", { exact: true })).toBeVisible();
  await page.getByText(/write file Finished/).click();
  await expect(page.getByText(/A private fixture note/, { exact: false }).last()).toBeVisible();
  expect(frames.some(frame => frame.includes("fixture-opaque-provider-state"))).toBe(false);
  expect(frames.some(frame => frame.includes("fixture-model-key"))).toBe(false);
  const otherContext = await browser.newContext();
  try {
    const other = await otherContext.newPage();
    await signup(other, `other-${Date.now()}@example.test`);
    await other.goto(threadURL);
    await expect(other.getByLabel("Message Chatty")).toBeVisible();
    await expect(other.getByText("Fixture answer: Hello", { exact: true })).toHaveCount(0);
    await expect(other.getByRole("button", { name: "Renamed conversation", exact: true })).toHaveCount(0);
    const otherSession = await (await other.request.get(`${process.env.CHATTY_TEST_URL}/api/session`)).json();
    const stolen = await other.request.post(`${process.env.CHATTY_TEST_URL}/api/send`, { headers: { origin: process.env.CHATTY_TEST_URL!, "x-snap-csrf": otherSession.csrf }, data: submitted });
    expect(stolen.status()).toBe(401);
  } finally { await otherContext.close(); }
  await page.request.get(`${process.env.CHATTY_FIXTURE_URL}/restart`);
  await page.reload();
  await expect(page.getByText("Saved your note.", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await page.getByRole("button", { name: "Confirm sign out", exact: true }).click();
  await expect(page.getByRole("link", { name: /Continue with Authy/ })).toBeVisible();
  expect(errors).toEqual([]);
});
