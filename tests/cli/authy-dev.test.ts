import { test, expect } from "bun:test";
import { chromium } from "@playwright/test";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { deployment } from "../support/deployment";

test("Authy dev retains failed builds and sessions across native/Wasm replacement", async () => {
  const root = resolve(import.meta.dir, "../..");
  await mkdir(`${root}/.tmp`, { recursive: true });
  const fixture = await mkdtemp(`${root}/.tmp/authy-dev-`);
  const browser = await chromium.launch();
  let child: ReturnType<typeof Bun.spawn> | undefined;
  let protocolContext: Awaited<ReturnType<typeof browser.newContext>> | undefined;
  let logs = "";
  async function until(predicate: () => boolean | Promise<boolean>) {
    const deadline = Date.now() + 60000;
    while (!await predicate()) {
      if (Date.now() > deadline || child?.exitCode !== null && child?.exitCode !== undefined) throw new Error(`Authy dev stalled:\n${logs}`);
      await Bun.sleep(25);
    }
  }
  try {
    for (const path of ["Cargo.toml", "Cargo.lock", "package.json", "tsconfig.json", "scripts", "crates", "platforms", "kits", "tools/cli", "apps", "tests/properties"])
      await cp(`${root}/${path}`, `${fixture}/${path}`, { recursive: true, filter: path => !/(^|\/)(\.snap|\.deployment|node_modules|target|build|dist)(\/|$)/.test(path) });
    for (const path of ["node_modules", ".tools", "target"]) await symlink(`${root}/${path}`, `${fixture}/${path}`);
    const database = `${fixture}/authy.sqlite`;
    const setup = await deployment(`${fixture}/host`, { host: { mode: "development", listen: "127.0.0.1:0", data_dir: fixture, database: "authy.sqlite" }, app: { clients: [], auto_approve_domain: "" } });
    const migrate = Bun.spawn([`${root}/target/debug/authy`, "--migrate", "--config", setup.path], { env: setup.env, stdout: "ignore", stderr: "inherit" });
    expect(await migrate.exited).toBe(0);
    child = Bun.spawn([`${root}/target/debug/snap`, "dev", `${fixture}/apps/authy`, "--config", setup.path], { env: setup.env, stdout: "pipe", stderr: "pipe" });
    for (const stream of [child.stdout, child.stderr]) void (async () => { for await (const chunk of stream as ReadableStream<Uint8Array>) logs += new TextDecoder().decode(chunk); })();
    await until(() => logs.includes("Authy dev http"));
    const url = /Authy dev (http:\/\/[^\s]+)/.exec(logs)![1];
    const page = await browser.newPage();
    await page.goto(url);
    await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
    await page.getByLabel("Email", { exact: true }).fill("dev-authy@example.test");
    await page.getByLabel("Password", { exact: true }).fill("Authy dev password");
    await page.getByRole("button", { name: "Create account", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("Survives rebuild");
    await page.getByRole("button", { name: "Save profile", exact: true }).click();
    await page.getByText("Saved revision 2", { exact: true }).waitFor();
    const css = `${fixture}/apps/authy/web/style.css`;
    await writeFile(css, await readFile(css,"utf8") + "\nbody { --authy-probe: active; }\n");
    await until(() => page.evaluate(() => getComputedStyle(document.body).getPropertyValue("--authy-probe").trim() === "active"));
    const source = `${fixture}/apps/authy/src/lib.rs`;
    const original = await readFile(source,"utf8");
    await writeFile(source, original + '\ncompile_error!("deliberate Authy dev failure");\n');
    await until(() => logs.includes("Rebuild failed; previous generation retained"));
    await page.reload();
    expect(await page.getByLabel("Name", { exact: true }).inputValue()).toBe("Survives rebuild");
    const ready = (logs.match(/generation ready/g) ?? []).length;
    const reloaded = page.waitForEvent("load");
    await writeFile(source, original);
    await until(() => (logs.match(/generation ready/g) ?? []).length > ready);
    await reloaded;
    await page.getByLabel("Name", { exact: true }).waitFor();
    await until(async () => await page.getByLabel("Name", { exact: true }).inputValue() === "Survives rebuild");
    await page.evaluate(() => { (window as any).__authyDev = "same"; });
    const ui = `${fixture}/apps/authy/web/pages/account.tsx`;
    await writeFile(ui, (await readFile(ui,"utf8")).replace("Manage your profile and active sessions.", "Manage your profile and active sessions after reload."));
    await page.getByText("Manage your profile and active sessions after reload.", { exact: true }).waitFor();
    expect(await page.evaluate(() => (window as any).__authyDev)).toBe("same");
    protocolContext = await browser.newContext({ javaScriptEnabled: false });
    const protocol = await protocolContext.newPage();
    await protocol.goto(`${url}/oauth/authorize?client_id=unknown`);
    await protocol.getByText("Return to the application and try again, or check your Authy session.", { exact: true }).waitFor();
    const sharedUI = `${fixture}/apps/authy/web/auth-ui.tsx`;
    const serverPages = (logs.match(/generation ready/g) ?? []).length;
    await writeFile(sharedUI, (await readFile(sharedUI, "utf8")).replace("Return to the application and try again, or check your Authy session.", "Updated server-rendered authorization guidance."));
    await until(() => (logs.match(/generation ready/g) ?? []).length > serverPages);
    await protocol.reload();
    await protocol.getByText("Updated server-rendered authorization guidance.", { exact: true }).waitFor();
    child.kill("SIGTERM"); expect(await child.exited).toBe(143);
    await until(async () => { try { await fetch(url); return false; } catch { return true; } });
  } catch (error) { console.error(logs); throw error; }
  finally { if (child && child.exitCode === null) { child.kill("SIGTERM"); await child.exited; } await protocolContext?.close(); await browser.close(); await rm(fixture,{recursive:true,force:true}); }
},120000);
