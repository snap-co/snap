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

test("WebSocket mutations synchronize conversations, enforce Access, and survive restart", async ({ page, browser }) => {
  const login = await page.request.get(`${process.env.CHATTY_TEST_URL}/auth/login`, {
    maxRedirects: 0, headers: { "x-snap-dev-origin": "http://evil.test" },
  });
  expect(new URL(login.headers().location!).searchParams.get("redirect_uri")).toBe(`${process.env.CHATTY_TEST_URL}/auth/callback`);
  const errors: string[] = [];
  const frames: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("websocket", socket => socket.on("framereceived", event => frames.push(String(event.payload))));
  await signup(page, `chatty-${Date.now()}@example.test`);
  await send(page, "Hello");
  await expect(page.locator(".transcript .user-message p")).toHaveText("Hello");
  await expect(page).toHaveURL(/\?thread=.+/);
  const threadURL = page.url();
  page.once("dialog", dialog => dialog.accept("Renamed conversation"));
  await page.getByRole("button", { name: "Rename", exact: true }).click();
  await expect(page.locator(".topbar strong")).toHaveText("Renamed conversation");
  await page.reload();
  await expect(page.locator(".topbar strong")).toHaveText("Renamed conversation");
  await expect(page.getByText("Hello", { exact: true })).toBeVisible();
  const second = await page.context().newPage();
  await second.goto(threadURL);
  await expect(second.getByText("Hello", { exact: true })).toBeVisible();
  await send(second, "From another client");
  await expect(page.getByText("From another client", { exact: true })).toBeVisible();
  await second.close();
  expect((await page.request.post(`${process.env.CHATTY_TEST_URL}/api/send`, { data: {} })).ok()).toBe(false);
  const otherContext = await browser.newContext();
  try {
    const other = await otherContext.newPage();
    await signup(other, `other-${Date.now()}@example.test`);
    await other.goto(threadURL);
    await expect(other.getByLabel("Message Chatty")).toBeVisible();
    await expect(other.getByText("Hello", { exact: true })).toHaveCount(0);
    await expect(other.getByRole("button", { name: "Renamed conversation", exact: true })).toHaveCount(0);
    const stolen = await other.evaluate(thread => new Promise<{ accepted: boolean; failed: boolean }>((resolve, reject) => {
      const socket = new WebSocket(`${location.origin.replace(/^http/, "ws")}/transport`);
      const timer = setTimeout(() => { socket.close(); reject(new Error("Timed out")); }, 5000);
      let accepted = false;
      socket.onopen = () => socket.send(JSON.stringify({ Connect: { bearer: "", client_id: crypto.randomUUID() } }));
      socket.onmessage = event => {
        const frame = JSON.parse(String(event.data));
        if (frame.Attached) socket.send(JSON.stringify({ Invoke: { id: 1, operation: "chatty.send", input: { thread_id: thread, request_id: "stolen", message: "unauthorized" } } }));
        for (const event of frame.Events ?? []) {
          if (event.Accepted) accepted = true;
          if (event.Completed) { clearTimeout(timer); socket.close(); resolve({ accepted, failed: "Err" in event.Completed.outcome }); }
        }
      };
    }), new URL(threadURL).searchParams.get("thread"));
    expect(stolen).toEqual({ accepted: false, failed: true });
  } finally { await otherContext.close(); }
  await page.request.get(`${process.env.CHATTY_FIXTURE_URL}/restart`);
  await page.reload();
  await expect(page.getByText("From another client", { exact: true })).toBeVisible();
  expect(frames.some(frame => frame.includes("Accepted"))).toBe(true);
  page.once("dialog", dialog => dialog.accept());
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("button", { name: "Renamed conversation", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await page.getByRole("button", { name: "Confirm sign out", exact: true }).click();
  await expect(page.getByRole("link", { name: /Continue with Authy/ })).toBeVisible();
  expect(errors).toEqual([]);
});
