import { mkdir, copyFile } from "node:fs/promises";
import { resolve } from "node:path";

// Called by both the Rust CLI and the release build. Every input is explicit.
const [application, host, html, wasm, output] = process.argv.slice(2);
if (!application || !host || !html || !wasm || !output)
  throw new Error(
    "Usage: build-web.ts <application> <host> <html> <wasm> <output>",
  );
const outdir = resolve(output);
await mkdir(outdir, { recursive: true });
const result = await Bun.build({
  entrypoints: [resolve(host)],
  naming: "main.[ext]",
  outdir,
  target: "browser",
  minify: true,
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  plugins: [
    {
      name: "application",
      setup(build) {
        build.onResolve({ filter: /^snap:application$/ }, () => ({
          path: resolve(application),
        }));
      },
    },
  ],
});
if (!result.success)
  throw new AggregateError(result.logs, "Browser build failed");
await copyFile(resolve(html), resolve(outdir, "index.html"));
await copyFile(resolve(wasm), resolve(outdir, "snap_client_wasm_bg.wasm"));
console.log(`Built browser application in ${outdir}`);
