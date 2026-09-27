import { test, expect } from "bun:test";
import { chromium } from "@playwright/test";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

// Explicit cross-process gate. Edits happen only in a disposable source copy.
test("dev keeps failed builds live, reloads Rust/Wasm and hot-replaces CSS", async () => {
  const root = resolve(import.meta.dir, "../..");
  await mkdir(`${root}/.tmp`, { recursive: true });
  const fixture = await mkdtemp(`${root}/.tmp/dev-cli-`);
  let child: ReturnType<typeof Bun.spawn> | undefined;
  const browser = await chromium.launch();
  let logs = "";
  async function until(predicate: () => boolean | Promise<boolean>) {
    const deadline = Date.now() + 60000;
    while (!await predicate()) {
      if (Date.now() > deadline || (child && child.exitCode !== null))
        throw new Error(`Dev gate did not complete:\n${logs}`);
      await Bun.sleep(30);
    }
  }
  try {
    for (const path of ["Cargo.toml", "Cargo.lock", "package.json", "tsconfig.json", "scripts", "crates",
      "platforms", "tools/cli", "apps/testy", "tests/properties"])
      await cp(`${root}/${path}`, `${fixture}/${path}`, {
        recursive: true, filter: path => !/(^|\/)(\.snap|node_modules|target)(\/|$)/.test(path),
      });
    // Share build caches/tools, never application data or editable source.
    for (const path of ["node_modules", ".tools", "target"])
      await symlink(`${root}/${path}`, `${fixture}/${path}`);
    const database = `${fixture}/identity.sqlite`;
    const migration = Bun.spawn([`${root}/target/debug/snap`, "migrate", "--database", database,
      "--migrations", `${fixture}/crates/identity/migrations`], { stdout: "ignore", stderr: "inherit" });
    expect(await migration.exited).toBe(0);
    child = Bun.spawn([`${root}/target/debug/snap`, "dev", `${fixture}/apps/testy`], {
      env: { ...process.env, TESTY_DATABASE: database, TESTY_WEB_ADDR: "127.0.0.1:0" },
      stdout: "pipe", stderr: "pipe",
    });
    for (const stream of [child.stdout, child.stderr]) void (async () => {
      for await (const chunk of stream as ReadableStream<Uint8Array>) logs += new TextDecoder().decode(chunk);
    })();
    await until(() => logs.includes("Testy dev http"));
    const url = /Testy dev (http:\/\/[^\s]+)/.exec(logs)![1];
    const page = await browser.newPage();
    const errors: string[] = [];
    page.on("pageerror", error => { errors.push(error.message); logs += `\nBrowser: ${error.message}`; });
    await page.goto(`${url}/calc`);
    await page.getByLabel("Email", { exact: true }).fill("dev@example.com");
    await page.getByLabel("Password", { exact: true }).fill("password123");
    await page.getByRole("button", { name: "Create account", exact: true }).click();
    await page.getByText("Connected", { exact: true }).waitFor();
    await page.getByLabel("Operand", { exact: true }).fill("12");
    await page.getByRole("button", { name: "+", exact: true }).click();
    await until(async () => await page.getByTestId("accumulator").textContent() === "12");
    const stylesheet = `${fixture}/apps/testy/web/style.css`;
    await writeFile(stylesheet, await readFile(stylesheet, "utf8") + "\nbody { --dev-probe: active; }\n");
    await until(() => page.evaluate(() => getComputedStyle(document.body).getPropertyValue("--dev-probe").trim() === "active"));
    expect(await page.getByTestId("accumulator").textContent()).toBe("12");
    expect((await fetch(`${url}/__dev`, { headers: { Origin: "http://elsewhere.invalid" } })).status).toBe(404);

    const source = `${fixture}/apps/testy/src/lib.rs`;
    const original = await readFile(source, "utf8");
    await writeFile(source, original + '\ncompile_error!("dev gate deliberate failure");\n');
    await until(() => logs.includes("Rebuild failed; previous generation retained"));
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    expect(await page.getByTestId("accumulator").textContent()).toBe("12");
    const ready = (logs.match(/generation ready/g) ?? []).length;
    await writeFile(source, original);
    await until(() => (logs.match(/generation ready/g) ?? []).length > ready);
    await page.getByText("Connected", { exact: true }).waitFor();
    await until(async () => await page.getByTestId("accumulator").textContent() === "0");
    // Component edits are React refresh, not document navigation.
    await page.evaluate(() => { (window as any).__devDocument = "same"; });
    const screens = `${fixture}/apps/testy/web/screens.tsx`;
    await writeFile(screens, (await readFile(screens, "utf8")).replace("SNAP / TESTY", "SNAP / RELOADED"));
    await page.getByText("SNAP / RELOADED", { exact: true }).waitFor();
    expect(await page.evaluate(() => (window as any).__devDocument)).toBe("same");
    expect(errors).toEqual([]);

    child.kill("SIGTERM");
    expect(await child.exited).toBe(143);
    await until(async () => {
      try { await fetch(url); return false; } catch { return true; }
    });
  } catch (error) {
    console.error(logs);
    throw error;
  } finally {
    if (child && child.exitCode === null) { child.kill("SIGTERM"); await child.exited; }
    await browser.close();
    await rm(fixture, { recursive: true, force: true });
  }
}, 120000);
