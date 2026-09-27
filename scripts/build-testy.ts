import { mkdir, copyFile } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
process.chdir(root);
async function run(command: string[]) {
  const child = Bun.spawn(command, { stdout: "inherit", stderr: "inherit" });
  if ((await child.exited) !== 0)
    throw new Error(`Failed: ${command.join(" ")}`);
}
const version = (await Bun.file("Cargo.toml").text()).match(
  /wasm-bindgen = "=([^"]+)"/,
)![1];
const candidates = [
  process.env.WASM_BINDGEN,
  Bun.which("wasm-bindgen"),
  resolve(`.tools/wasm-bindgen-${version}/bin/wasm-bindgen`),
].filter((path): path is string => !!path);
let bindgen: string | undefined;
for (const candidate of candidates) {
  if (!(await Bun.file(candidate).exists())) continue;
  const check = Bun.spawn([candidate, "--version"], { stdout: "pipe" });
  if (
    (await new Response(check.stdout).text()).trim() ===
      `wasm-bindgen ${version}` &&
    (await check.exited) === 0
  ) {
    bindgen = candidate;
    break;
  }
}
if (!bindgen) {
  const tools = resolve(`.tools/wasm-bindgen-${version}`);
  await run([
    "mise",
    "exec",
    "--",
    "cargo",
    "install",
    "wasm-bindgen-cli",
    "--version",
    version,
    "--locked",
    "--root",
    tools,
  ]);
  bindgen = `${tools}/bin/wasm-bindgen`;
}
await run([
  "mise",
  "exec",
  "--",
  "cargo",
  "build",
  "-p",
  "testy-wasm",
  "--target",
  "wasm32-unknown-unknown",
]);
const outdir = resolve("apps/testy/.snap/web");
await mkdir(outdir, { recursive: true });
await run([
  bindgen,
  "--target",
  "web",
  "--out-dir",
  `${outdir}/bindings`,
  "target/wasm32-unknown-unknown/debug/testy_wasm.wasm",
]);
const result = await Bun.build({
  entrypoints: ["apps/testy/web/app.tsx"],
  outdir,
  target: "browser",
  sourcemap: "linked",
  naming: "[name].[ext]",
  define: { "process.env.NODE_ENV": JSON.stringify("development") },
});
if (!result.success)
  throw new AggregateError(result.logs, "Testy web build failed");
await copyFile("apps/testy/web/index.html", `${outdir}/index.html`);
console.log(`Testy web assets: ${outdir}`);
