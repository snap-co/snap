import { mkdir, copyFile, cp } from "node:fs/promises";
import { resolve } from "node:path";

export async function build(app: "authy" | "chatty" | "factorio", entry: "app" | "main") {
  const root = resolve(import.meta.dir, ".."); process.chdir(root);
  async function run(command: string[]) {
    const child = Bun.spawn(command, { stdout: "inherit", stderr: "inherit" });
    if (await child.exited !== 0) throw new Error(`Failed: ${command.join(" ")}`);
  }
  const version = (await Bun.file("Cargo.toml").text()).match(/wasm-bindgen = "=([^"]+)"/)![1];
  let bindgen: string | undefined;
  for (const candidate of [process.env.WASM_BINDGEN, Bun.which("wasm-bindgen"), resolve(`.tools/wasm-bindgen-${version}/bin/wasm-bindgen`)]) {
    if (!candidate || !await Bun.file(candidate).exists()) continue;
    const child = Bun.spawn([candidate, "--version"], { stdout: "pipe" });
    if ((await new Response(child.stdout).text()).trim() === `wasm-bindgen ${version}` && await child.exited === 0) { bindgen = candidate; break; }
  }
  if (!bindgen) {
    const tools = resolve(`.tools/wasm-bindgen-${version}`);
    await run(["mise", "exec", "--", "cargo", "install", "wasm-bindgen-cli", "--version", version, "--locked", "--root", tools]);
    bindgen = `${tools}/bin/wasm-bindgen`;
  }
  await run(["mise", "exec", "--", "cargo", "build", "-p", `${app}-wasm`, "--target", "wasm32-unknown-unknown"]);
  const outdir = resolve(process.env[`${app.toUpperCase()}_BUILD_DIR`] ?? `apps/${app}/.snap/web`);
  await mkdir(outdir, { recursive: true });
  await run([bindgen, "--target", "web", "--out-dir", `${outdir}/bindings`, `target/wasm32-unknown-unknown/debug/${app}_wasm.wasm`]);
  if (!process.argv.includes("--bindings-only")) {
    const result = await Bun.build({ entrypoints: [`apps/${app}/web/${entry}.tsx`], outdir, target: "browser", sourcemap: "linked", naming: "[name].[ext]", define: { "process.env.NODE_ENV": JSON.stringify("development") } });
    if (!result.success) throw new AggregateError(result.logs, `${app} web build failed`);
    await copyFile(`apps/${app}/web/index.html`, `${outdir}/index.html`);
  }
  if (process.argv.includes("--package")) {
    await run(["mise", "exec", "--", "cargo", "build", "-p", `${app}-native`]);
    await mkdir(`dist/${app}`, { recursive: true });
    await copyFile(`target/debug/${app}`, `dist/${app}/${app}`);
    await cp(outdir, `dist/${app}/web`, { recursive: true });
  }
  console.log(`${app} web assets: ${outdir}`);
}
