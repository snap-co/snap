// Framework-owned frontend build. Apps provide sources, not build commands.
import { copyFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [app, output, mode, kind, library] = process.argv.slice(2);
const production = mode === "production";
const options = { target: "browser", sourcemap: production ? "none" : "linked", minify: production,
  naming: { entry: "[name].[ext]", chunk: "chunks/[name]-[hash].[ext]", asset: "assets/[name]-[hash].[ext]" },
  define: { "process.env.NODE_ENV": JSON.stringify(mode) },
  plugins: [{ name: "snap-generated-bindings", setup(build) {
    build.onResolve({ filter: /^@snap\/wasm$/ }, () =>
      ({ path: join(output, "bindings", `${library}.js`) }));
  } }] };
async function build(entrypoints, outdir, extra = {}) {
  const result = await Bun.build({ ...options, ...extra, entrypoints, outdir });
  if (!result.success) throw new AggregateError(result.logs, "Frontend build failed");
}
const serverAssets = join(app, "web/server.tsx");
if (await Bun.file(serverAssets).exists()) {
  const assets = await (await import(pathToFileURL(serverAssets).href)).default();
  for (const [name, content] of Object.entries(assets)) {
    if (!/^[a-zA-Z0-9_.-]+$/.test(name)) throw new Error("Server asset must be a filename");
    await Bun.write(join(output, name), content);
  }
}
await build([join(app, "web/app.tsx")], output);
await copyFile(join(app, "web/index.html"), join(output, "index.html"));
if (kind === "package") {
  for (const [source, name] of [["cli.ts", "cli.js"], ["native/bridge.ts", "bridge.js"], ["client.ts", "client.js"]]) {
    if (await Bun.file(join(app, source)).exists()) {
      await build([join(app, source)], join(output, ".."), { target: "bun", naming: name });
    }
  }
}
