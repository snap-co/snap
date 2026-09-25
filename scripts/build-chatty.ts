import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
const root = resolve(import.meta.dirname, "..");
const outdir = resolve(root, "apps/chatty/.snap/web");
const build = await Bun.build({ entrypoints: [resolve(root, "apps/chatty/web/main.tsx")], outdir, target: "browser", minify: true, naming: "[name].[ext]", define: { "process.env.NODE_ENV": JSON.stringify("production") } });
if (!build.success) throw new Error(build.logs.join("\n"));
await mkdir(outdir, { recursive: true });
await writeFile(resolve(outdir, "index.html"), '<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Chatty</title><link rel="stylesheet" href="/main.css"></head><body><div id="root"></div><script type="module" src="/main.js"></script></body></html>');
console.log(`Chatty browser assets: ${outdir}`);
