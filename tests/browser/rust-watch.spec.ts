import { test, expect } from "@playwright/test";
import { readFile, writeFile, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { healthyProject } from "../adapters/project";
import { startServer } from "../adapters/server";

test("Rust edits restart/reload, retain failed builds, and recover without stale generations", async ({ page }) => {
  test.setTimeout(180_000);
  const project = await healthyProject(true);
  const native = resolve(project.directory, "rust/native/src/main.rs");
  const wasm = resolve(project.directory, "rust/wasm/src/lib.rs");
  const shared = resolve(project.directory, "rust/app/src/lib.rs");
  const sharedSource = await readFile(shared, "utf8");
  await writeFile(shared, `${sharedSource}\npub fn dev_marker() -> &'static str { "shared-one" }\n`);
  const nativeSource = (await readFile(native, "utf8")).replace("    let application =", `
    use std::io::Write;
    writeln!(std::fs::OpenOptions::new().create(true).append(true).open(".snap/starts")?, "started")?;
    let application =`);
  await writeFile(native, nativeSource);
  const wasmSource = `${await readFile(wasm, "utf8")}\n#[wasm_bindgen]\npub fn dev_marker() -> String { format!("wasm-one:{}", healthy::dev_marker()) }\n`;
  await writeFile(wasm, wasmSource);
  const facade = resolve(project.directory, "client.ts");
  await writeFile(facade, (await readFile(facade, "utf8"))
    .replace("Client as WasmClient,", "dev_marker, Client as WasmClient,")
    .replace("  return observeClient(", '  document.body.dataset.wasm = dev_marker();\n  return observeClient('));
  // A fixture-owned build hook makes an edit during compilation deterministic.
  await writeFile(resolve(project.directory, "gate.py"), `import time\nfrom pathlib import Path\nif Path(".snap/block").exists():\n Path(".snap/building").touch()\n deadline=time.monotonic()+20\n while Path(".snap/block").exists() and time.monotonic()<deadline: time.sleep(0.02)\n`);
  const config = resolve(project.directory, "snap.toml");
  await writeFile(config, `${await readFile(config, "utf8")}\n[prepare]\nbuild=[["python3","gate.py"]]\n`);
  let server: Awaited<ReturnType<typeof startServer>> | undefined;
  const starts = async () => (await readFile(resolve(project.directory, ".snap/starts"), "utf8")).trim().split("\n").length;
  try {
    server = await startServer({ dev: true, project: project.directory, env: project.env, freshBuild: true });
    const build = async () => (await (await fetch(`${server!.baseUrl}/__snap/build`)).json()).build as string;
    await page.goto(server.baseUrl);
    await expect(page.getByRole("status")).toHaveText("OK");
    await expect(page.locator("body")).toHaveAttribute("data-wasm", "wasm-one:shared-one");
    const initial = await build();
    const initialStarts = await starts();
    await page.evaluate(() => { (window as any).lifetime = "old"; });
    await writeFile(native, `${nativeSource}\n// native edit\n`);
    await expect.poll(build, { timeout: 30_000 }).not.toBe(initial);
    // This assertion intentionally crosses a navigation. Playwright retries the
    // function in the new document if reload destroys the old execution context.
    await page.waitForFunction(() => (window as any).lifetime === undefined, undefined, { timeout: 30_000 });
    await expect(page.getByRole("status")).toHaveText("OK");
    expect(await starts()).toBe(initialStarts + 1);
    const nativeBuild = await build();
    await writeFile(wasm, wasmSource.replace("wasm-one", "wasm-two"));
    await expect(page.locator("body")).toHaveAttribute("data-wasm", "wasm-two:shared-one", { timeout: 30_000 });
    expect(await build()).toBe(nativeBuild);
    expect(await starts()).toBe(initialStarts + 1);

    await page.evaluate(() => { (window as any).lifetime = "working"; });
    await writeFile(shared, "invalid Rust source\n");
    await expect.poll(() => server!.logs(), { timeout: 30_000 }).toContain("Rebuild failed; previous generation retained");
    expect(await build()).toBe(nativeBuild);
    expect(await page.evaluate(() => (window as any).lifetime)).toBe("working");
    await expect(page.getByRole("status")).toHaveText("OK");
    await writeFile(shared, `${sharedSource}\npub fn dev_marker() -> &'static str { "shared-two" }\n`);
    await expect(page.locator("body")).toHaveAttribute("data-wasm", "wasm-two:shared-two", { timeout: 30_000 });
    await expect(page.getByRole("status")).toHaveText("OK");
    expect(await build()).not.toBe(nativeBuild);

    const beforeSlow = await starts();
    await writeFile(resolve(project.directory, ".snap/block"), "");
    await writeFile(native, `${nativeSource}\n// intermediate edit\n`);
    await expect.poll(() => readFile(resolve(project.directory, ".snap/building"), "utf8").then(() => true).catch(() => false)).toBe(true);
    await writeFile(native, `${nativeSource}\n// latest edit\n`);
    await rm(resolve(project.directory, ".snap/block"));
    await expect.poll(() => server!.logs(), { timeout: 30_000 }).toContain("Discarding superseded Rust generation");
    await expect.poll(starts, { timeout: 30_000 }).toBe(beforeSlow + 1);
    await expect(page.getByRole("status")).toHaveText("OK");

    const beforeFailure = await build();
    // HTTP readiness precedes acceptance while Snap checks for superseding edits.
    // Do not inject the next failure into that still-provisional version.
    await expect.poll(() => server!.logs(), { timeout: 30_000 }).toContain(`Rust generation ready: ${beforeFailure}`);
    await writeFile(native, nativeSource.replace("    use std::io::Write;", "    std::process::exit(37);\n    use std::io::Write;"));
    await expect.poll(() => server!.logs(), { timeout: 30_000 }).toContain("Replacement failed; restoring previous generation");
    await expect.poll(build, { timeout: 30_000 }).toBe(beforeFailure);
    await expect(page.getByRole("status")).toHaveText("OK");
    await writeFile(native, nativeSource);
    await expect.poll(build, { timeout: 30_000 }).not.toBe(beforeFailure);
    await expect(page.getByRole("status")).toHaveText("OK");

    const configBefore = await build();
    const validConfig = await readFile(config, "utf8");
    await writeFile(config, "invalid config");
    await expect.poll(() => server!.logs()).toContain("Configuration failed; previous generation retained");
    expect(await build()).toBe(configBefore);
    await writeFile(config, `${validConfig}\n# corrected configuration\n`);
    await expect.poll(async () => {
      try { return await build(); } catch { return configBefore; }
    }, { timeout: 30_000 }).not.toBe(configBefore);
    await expect(page.getByRole("status")).toHaveText("OK");

    // Shutdown while a build hook is waiting must release it and both servers.
    await rm(resolve(project.directory, ".snap/building"));
    await writeFile(resolve(project.directory, ".snap/block"), "");
    const manifest = resolve(project.directory, "rust/native/Cargo.toml");
    await writeFile(manifest, `${await readFile(manifest, "utf8")}\n# manifest edit\n`);
    await expect.poll(() => readFile(resolve(project.directory, ".snap/building"), "utf8").then(() => true).catch(() => false)).toBe(true);
  } catch (error) {
    console.error(server?.logs());
    throw error;
  } finally {
    try {
      await page.close();
    } finally {
      try { await server?.close(); } finally { await project.close(); }
    }
  }
  await expect(fetch(`${server!.baseUrl}/health/up`)).rejects.toThrow();
  await expect(fetch(`${server!.backendUrl}/health/up`)).rejects.toThrow();
});
