import { test, expect } from "bun:test";
import { chromium } from "@playwright/test";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pair } from "../../apps/chatty/tests/support/pair";

test("Chatty dev retains OAuth sessions and threads across failed builds and native/Wasm replacement", async () => {
  const root = resolve(import.meta.dir, "../..");
  await mkdir(`${root}/.tmp`, { recursive: true });
  const fixture = await mkdtemp(`${root}/.tmp/chatty-dev-`);
  const browser = await chromium.launch();
  let server: Awaited<ReturnType<typeof pair>> | undefined;
  async function until(predicate: () => boolean | Promise<boolean>) {
    const deadline = Date.now() + 60000;
    while (!await predicate()) {
      if (Date.now() > deadline) throw new Error(`Chatty dev stalled:\n${server?.logs}`);
      await Bun.sleep(25);
    }
  }
  try {
    for (const path of ["Cargo.toml", "Cargo.lock", "package.json", "tsconfig.json", "scripts", "crates", "platforms", "tools/cli", "apps", "tests/properties"])
      await cp(`${root}/${path}`, `${fixture}/${path}`, { recursive: true, filter: path => !/(^|\/)(\.snap|node_modules|target|build)(\/|$)/.test(path) });
    for (const path of ["node_modules", ".tools", "target"]) await symlink(`${root}/${path}`, `${fixture}/${path}`);
    server = await pair({ root: fixture, dev: true });
    const page = await browser.newPage();
    await page.goto(server.base);
    await page.getByRole("link", { name: /Continue with Authy/ }).click();
    await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
    await page.getByLabel("Email", { exact: true }).fill("dev-chatty@example.test");
    await page.getByLabel("Password", { exact: true }).fill("Chatty dev OAuth password");
    await page.getByRole("button", { name: "Create account", exact: true }).click();
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await page.getByLabel("Message Chatty").fill("Persistent dev thread");
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await page.locator(".transcript .user-message p").filter({ hasText: "Persistent dev thread" }).waitFor();
    const css = `${fixture}/apps/chatty/web/style.css`;
    await writeFile(css, await readFile(css, "utf8") + "\nbody { --chatty-probe: active; }\n");
    await until(() => page.evaluate(() => getComputedStyle(document.body).getPropertyValue("--chatty-probe").trim() === "active"));
    const source = `${fixture}/apps/chatty/src/lib.rs`;
    const original = await readFile(source, "utf8");
    await writeFile(source, original + '\ncompile_error!("deliberate Chatty build failure");\n');
    await until(() => server!.logs.includes("Rebuild failed; previous generation retained"));
    await page.reload();
    await page.locator(".transcript .user-message p").filter({ hasText: "Persistent dev thread" }).waitFor();
    const ready = (server.logs.match(/generation ready/g) ?? []).length;
    const reloaded = page.waitForEvent("load");
    await writeFile(source, original);
    await until(() => (server!.logs.match(/generation ready/g) ?? []).length > ready);
    await reloaded;
    await page.locator(".transcript .user-message p").filter({ hasText: "Persistent dev thread" }).waitFor();
    await page.evaluate(() => { (window as any).__chattyDev = "same"; });
    const ui = `${fixture}/apps/chatty/web/main.tsx`;
    await writeFile(ui, (await readFile(ui, "utf8")).replace("YOUR CONVERSATIONS", "UPDATED CONVERSATIONS"));
    await page.getByText("UPDATED CONVERSATIONS", { exact: true }).waitFor();
    expect(await page.evaluate(() => (window as any).__chattyDev)).toBe("same");
    await server.stop(); expect(server.child.exitCode).toBe(143);
    await until(async () => { try { await fetch(server!.base); return false; } catch { return true; } });
  } catch (error) { console.error(server?.logs); throw error; }
  finally { await server?.close(); await browser.close(); await rm(fixture, { recursive: true, force: true }); }
}, 180000);
