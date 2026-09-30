import { test, expect, type Page } from "@playwright/test";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { readFile, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";

const exec = promisify(execFile);
const base = process.env.FACTORIO_TEST_URL!, directory = process.env.FACTORIO_FIXTURE_DIR!;
const binary = resolve(import.meta.dirname, "../../../../target/debug/factorio");
const env = { ...process.env, FACTORIO_TOKEN: "" };
async function cli(path: string, ...args: string[]) {
  const result = await exec(binary, ["--credentials", path, ...args], { env, timeout: 70000 });
  return JSON.parse(result.stdout);
}
async function authority(owner: string, action = "state") {
  const response = await fetch(`${process.env.FACTORIO_FIXTURE_URL}/cli-authority`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ owner, action }),
  });
  expect(response.ok).toBe(true);
  return response.json();
}
async function login(page: Page) {
  await page.goto(base);
  await page.getByRole("link", { name: "Continue with Authy" }).click();
  await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
  await page.getByLabel("Email", { exact: true }).fill(`cli-${randomUUID()}@example.test`);
  await page.getByLabel("Password", { exact: true }).fill("Long-lived CLI fixture password");
  await page.getByRole("button", { name: "Create account", exact: true }).click();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Create your first workspace", exact: true })).toBeVisible();
  const path = `${directory}/login-${randomUUID()}.json`;
  const child = spawn(binary, ["login", "--credentials", path], { env });
  let logs = "", output = "";
  child.stderr.on("data", data => logs += data.toString());
  child.stdout.on("data", data => output += data.toString());
  const exit = new Promise<number | null>((resolve, reject) => { child.once("exit", resolve); child.once("error", reject); });
  try {
    await expect.poll(() => logs.includes("Waiting for browser approval")).toBe(true);
    await page.goto(logs.match(/Open (http[^\s]+)/)![1]!);
    await page.getByRole("button", { name: "Allow CLI access" }).click();
    expect(await exit).toBe(0);
    const result = JSON.parse(output);
    expect(result.logged_in).toBe(true);
    expect(result.bearer).toBeUndefined();
    return { path, owner: result.owner as string, expires: result.expires as number };
  } finally { if (child.exitCode === null) { child.kill(); await exit; } }
}

test("saved login refreshes real Authy tokens after restart without a browser, then respects local expiry", async ({ page }) => {
  const granted = await login(page);
  expect(granted.expires).toBeGreaterThan(Math.floor(Date.now() / 1000) + 29 * 24 * 60 * 60);
  const saved = JSON.parse(await readFile(granted.path, "utf8"));
  expect(saved.refresh).toBeUndefined();
  await page.close();
  expect(await cli(granted.path, "workspaces")).toEqual([]);
  const before = await authority(granted.owner);
  expect(before.cli_expires).toBe(granted.expires);
  await authority(granted.owner, "expire-access");
  // Independent logical identities authenticate concurrently against one rotating
  // refresh family. They must not race refresh-token reuse or need browser traffic.
  const paths = [granted.path, ...[1, 2].map(n => `${granted.path}.${n}`)];
  for (const path of paths.slice(1)) await writeFile(path, JSON.stringify({ ...saved, client_id: randomUUID(), lifetime: null }), { mode: 0o600 });
  expect(await Promise.all(paths.map(path => cli(path, "workspaces")))).toEqual([[], [], []]);
  const after = await authority(granted.owner);
  expect(after.version).toBe(before.version + 1);
  expect(after.access_expires).toBeGreaterThan(Math.floor(Date.now() / 1000));
  expect(after.cli_expires).toBe(granted.expires);
  expect(JSON.parse(await readFile(granted.path, "utf8")).bearer).toBe(saved.bearer);
  await authority(granted.owner, "expire-login");
  await expect(cli(granted.path, "workspaces")).rejects.toThrow();
  expect((await authority(granted.owner)).version).toBe(after.version);
});

test("revoked Authy refresh grant rejects saved CLI login and retires its local session", async ({ page }) => {
  const granted = await login(page);
  await page.close();
  await authority(granted.owner, "revoke-grant");
  await expect(cli(granted.path, "workspaces")).rejects.toThrow();
  expect(await authority(granted.owner)).toEqual({ revoked: true });
});
