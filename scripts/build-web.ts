import { mkdir, copyFile } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const outdir = resolve(root, "dist/web");
await mkdir(outdir, { recursive: true });
const result = await Bun.build({
  entrypoints: [resolve(root, "clients/react/main.tsx")],
  outdir,
  target: "browser",
  minify: true,
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  plugins: [
    {
      name: "application",
      setup(build) {
        build.onResolve({ filter: /^snap:application$/ }, () => ({
          path: resolve(root, "apps/healthy/web/app.tsx"),
        }));
      },
    },
  ],
});
if (!result.success)
  throw new AggregateError(result.logs, "Browser build failed");
await copyFile(
  resolve(root, "clients/react/index.html"),
  resolve(outdir, "index.html"),
);
await copyFile(
  resolve(root, "clients/typescript/wasm/snap_client_wasm_bg.wasm"),
  resolve(outdir, "snap_client_wasm_bg.wasm"),
);
console.log(`Built Healthy browser application in ${outdir}`);
