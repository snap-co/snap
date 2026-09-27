import { test, expect } from "@playwright/test";
import { authyServer } from "../support/authy";
import { authyProject } from "../support/project";
import { startServer } from "../../../../tests/adapters/server";
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { workersServer } from "../../../../tests/adapters/workers";

test("Authy development keeps its public origin and socket after a Rust rebuild", async ({ page }) => {
  test.setTimeout(120_000);
  const project = await authyProject();
  let server: Awaited<ReturnType<typeof startServer>> | undefined;
  const traffic: string[] = [];
  let attachments = 0;
  page.on("websocket", socket => {
    if (!socket.url().includes("/_transport/ws")) return;
    attachments++;
    socket.on("framesent", event => traffic.push(`sent ${event.payload}`));
    socket.on("framereceived", event => traffic.push(`received ${event.payload}`));
    socket.on("close", () => traffic.push("closed"));
    socket.on("socketerror", error => traffic.push(`error ${error}`));
  });
  try {
    server = await startServer({ dev: true, project: project.directory, env: project.env, freshBuild: true });
    const build = await (await page.request.get(`${server.baseUrl}/__snap/build`)).json();
    const enrolled = await page.request.post(`${server.baseUrl}/account/create`, {
      headers: { "x-snap-build": build.build },
      data: { email: "idle@example.test", password: "original password" },
    });
    expect(enrolled.ok()).toBe(true);
    await page.goto(server.baseUrl);
    await expect(page.getByTestId("connection")).toHaveText("connected");
    await expect(page.getByText("idle@example.test", { exact: true })).toBeVisible();
    const native = resolve(project.directory, "rust/native/src/main.rs");
    await writeFile(native, `${await readFile(native, "utf8")}\n// trigger owned native rebuild\n`);
    await expect.poll(() => server!.logs(), { timeout: 30_000 }).toContain("Rust generation ready:");
    await page.reload();
    // The post-rebuild page must attach, then remain attached past the retry cap.
    await expect(page.getByTestId("connection")).toHaveText("connected", { timeout: 10_000 });
    await expect(page.getByText("idle@example.test", { exact: true })).toBeVisible();
    const stable = attachments;
    await page.waitForTimeout(11_000);
    expect(attachments, traffic.join("\n")).toBe(stable);
    await expect(page.getByTestId("connection")).toHaveText("connected");
    await page.getByRole("button", { name: "Sign out", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Sign in", exact: true })).toBeVisible();
  } catch (error) {
    console.error(server?.logs(), traffic.join("\n"));
    throw error;
  } finally { try { await page.close(); } finally { try { await server?.close(); } finally { await project.close(); } } }
});

for (const host of ["package", "development proxy", "workers"] as const) {
  test(`Authy ${host}: sign in, reload, reconnect and remote revocation`, async ({ browser }) => {
    test.setTimeout(120_000);
    const server = await (host === "workers" ? workersServer("authy") : authyServer(host === "development proxy"));
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
      if (host !== "development proxy") {
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
