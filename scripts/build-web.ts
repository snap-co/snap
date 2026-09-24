import { mkdir, copyFile } from "node:fs/promises";
import { resolve, dirname, relative, isAbsolute } from "node:path";

// Embedded by the Rust builder. Every input is explicit.
const [application, host, html, wasm, output, profile, bindingSource, bindingOutput] = process.argv.slice(2);
if (!application || !host || !html || !wasm || !output || !["debug", "release"].includes(profile))
  throw new Error(
    "Usage: build-web.ts <application> <host> <html> <wasm> <output> <debug|release>",
  );
const outdir = resolve(output);
await mkdir(outdir, { recursive: true });
const result = await Bun.build({
  entrypoints: [resolve(host)],
  naming: "main.[ext]",
  outdir,
  target: "browser",
  minify: profile === "release",
  define: { "process.env.NODE_ENV": JSON.stringify(profile === "release" ? "production" : "development") },
  plugins: [
    {
      name: "application",
      setup(build) {
        if (bindingSource && bindingOutput && bindingSource !== bindingOutput) {
          build.onResolve({ filter: /^\./ }, (args) => {
            const path = resolve(dirname(args.importer), args.path);
            const suffix = relative(bindingSource, path);
            if (!suffix.startsWith("..") && !isAbsolute(suffix))
              return { path: Bun.resolveSync(resolve(bindingOutput, suffix), bindingOutput) };
          });
        }
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
