import { test, expect } from "@playwright/test";
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { buildProject, healthyProject } from "../adapters/project";
import { startServer } from "../adapters/server";

test("standalone builds cannot replace a live dev session's JS/WASM pair", async ({ page }) => {
  test.setTimeout(180_000);
  const project = await healthyProject(true);
  const wasm = resolve(project.directory, "rust/wasm/src/lib.rs");
  const native = resolve(project.directory, "rust/native/src/main.rs");
  const source = `${await readFile(wasm, "utf8")}\ninclude!(concat!(env!("OUT_DIR"), "/ownership.rs"));\n`;
  await writeFile(wasm, source);
  // The separate command changes the generated binding ABI without editing watched
  // sources. Mixing its numeric wrapper with dev's string-returning WASM must fail.
  await writeFile(resolve(project.directory, "rust/wasm/build.rs"), `fn main() {
    println!("cargo:rerun-if-env-changed=SNAP_TEST_STANDALONE");
    let function = if std::env::var_os("SNAP_TEST_STANDALONE").is_some() {
        "#[wasm_bindgen] pub fn ownership_marker() -> u32 { 73 }"
    } else {
        r#"#[wasm_bindgen] pub fn ownership_marker() -> String { String::from("dev-owned") }"#
    };
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("ownership.rs"), function).unwrap();
}
`);
  const facade = resolve(project.directory, "client.ts");
  await writeFile(facade, (await readFile(facade, "utf8"))
    .replace("Client as WasmClient,", "ownership_marker, Client as WasmClient,")
    .replace("  return observeClient(", '  document.body.dataset.owner = String(ownership_marker());\n  return observeClient('));
  let server: Awaited<ReturnType<typeof startServer>> | undefined;
  try {
    server = await startServer({ dev: true, project: project.directory, env: project.env, freshBuild: true });
    const build = async () => (await (await fetch(`${server!.baseUrl}/__snap/build`)).json()).build as string;
    await page.goto(server.baseUrl);
    await expect(page.getByRole("status")).toHaveText("OK");
    await expect(page.locator("body")).toHaveAttribute("data-owner", "dev-owned");
    const initial = await build();
    for (const phase of ["initial", "reloaded"]) {
      await buildProject(project, { SNAP_TEST_STANDALONE: "1" });
      const packaged = await startServer({ executable: resolve(project.directory, ".snap/build/debug/healthy") });
      const packagePage = await page.context().newPage();
      try {
        await packagePage.goto(packaged.baseUrl);
        await expect(packagePage.getByRole("status")).toHaveText("OK");
        await expect(packagePage.locator("body")).toHaveAttribute("data-owner", "73");
      } finally {
        await packagePage.close();
        await packaged.close();
      }
      // A fresh page loads bindings again, including when Vite cached the first page.
      await page.reload();
      await expect(page.getByRole("status")).toHaveText("OK");
      await expect(page.locator("body")).toHaveAttribute("data-owner", "dev-owned");
      expect(await build()).toBe(initial);
      await page.evaluate(() => { (window as any).lifetime = "before-edit"; });
      if (phase === "initial") await writeFile(wasm, `${source}\n// WASM edit\n`);
      else await writeFile(native, `${await readFile(native, "utf8")}\n// native edit\n`);
      await expect.poll(() => page.evaluate(() => (window as any).lifetime), { timeout: 30_000 }).toBeUndefined();
      await expect(page.getByRole("status")).toHaveText("OK");
      await expect(page.locator("body")).toHaveAttribute("data-owner", "dev-owned");
      if (phase === "initial") expect(await build()).toBe(initial);
      else expect(await build()).not.toBe(initial);
    }
  } catch (error) {
    console.error(server?.logs());
    throw error;
  } finally {
    await page.goto("about:blank");
    await server?.close();
    await project.close();
  }
});
