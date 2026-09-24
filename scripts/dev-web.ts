import { createServer, request } from "node:http";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { createInterface } from "node:readline";
import { basename, dirname, isAbsolute, relative, resolve } from "node:path";
import type { Plugin, ViteDevServer } from "vite";

const [project, packageDir, application, host, html, initialWasm, backend, address, bindingSource, initialBindings] = process.argv.slice(2);
let wasm = initialWasm;
let bindings = initialBindings;
let generation = basename(dirname(initialBindings));
if (![project, packageDir, application, host, html, wasm, backend, address, bindingSource, bindings].every(Boolean))
  throw new Error("Missing Snap development server inputs");
// Resolve browser tooling from the configured JS package, not the embedded driver.
const require = createRequire(resolve(packageDir, "package.json"));
let vitePath: string, reactPath: string;
try {
  vitePath = require.resolve("vite");
  reactPath = require.resolve("@vitejs/plugin-react");
} catch (cause) {
  throw new Error("snap dev needs vite and @vitejs/plugin-react in web.package-dir; install them with bun add --dev", { cause });
}
const { createServer: createViteServer } = await import(vitePath);
const { default: react } = await import(reactPath);
const listen = new URL(`http://${address}`);
const backendUrl = new URL(backend);
let vite: ViteDevServer;
const server = createServer(async (req, res) => {
  try {
    const path = new URL(req.url ?? "/", "http://snap.local").pathname;
    if (path === "/__snap/build") res.setHeader("x-snap-dev-generation", generation);
    if (path === "/" || path === "/index.html") {
      const template = (await readFile(html, "utf8"))
        .replace(/<link\b[^>]*href=["']\/?main\.css["'][^>]*>/g, "")
        .replace(/src=["']\/?main\.js["']/g, `src="/@fs/${host}"`);
      res.setHeader("Content-Type", "text/html");
      res.setHeader("Cache-Control", "no-store");
      res.end(await vite.transformIndexHtml(req.url ?? "/", template));
      return;
    }
    if (path === "/snap_client_wasm_bg.wasm") {
      res.setHeader("Content-Type", "application/wasm");
      res.setHeader("Cache-Control", "no-store");
      res.end(await readFile(wasm));
      return;
    }
    vite.middlewares(req, res, () => {
      // Preserve the public Host/Origin and Set-Cookie headers. Browser SDK calls
      // stay on one origin while the native host owns all application HTTP routes.
      const upstream = request({
        hostname: backendUrl.hostname.replace(/^\[|\]$/g, ""), port: backendUrl.port || "80", path: req.url,
        method: req.method, headers: req.headers,
      }, (reply) => {
        res.writeHead(reply.statusCode ?? 502, reply.headers);
        reply.pipe(res);
      });
      upstream.on("error", () => { if (!res.headersSent) res.writeHead(502); res.end("Native host unavailable"); });
      req.on("aborted", () => upstream.destroy());
      res.on("close", () => upstream.destroy());
      req.pipe(upstream);
    });
  } catch (error) {
    console.error(error);
    if (!res.headersSent) res.writeHead(500);
    res.end("Development asset error");
  }
});
vite = await createViteServer({
  configFile: false,
  clearScreen: false,
  root: project,
  appType: "custom",
  cacheDir: resolve(dirname(initialBindings), "vite"),
  plugins: [{
    name: "snap-private-bindings",
    enforce: "pre",
    async resolveId(source, importer) {
      if (!importer || !source.startsWith(".")) return;
      const path = resolve(dirname(importer.split("?")[0]), source);
      const suffix = relative(bindingSource, path);
      if (!suffix.startsWith("..") && !isAbsolute(suffix)) {
        // Let Vite resolve extensions in the private directory. A missing private
        // module must fail here instead of falling back to published bindings.
        const target = resolve(bindings, suffix);
        const resolved = await this.resolve(target, importer, { skipSelf: true });
        if (!resolved) this.error(`Private binding module not found: ${target}`);
        return resolved;
      }
    },
  } satisfies Plugin, react()],
  resolve: { alias: { "snap:application": application }, dedupe: ["react", "react-dom"] },
  optimizeDeps: { include: ["react", "react-dom/client", "react/jsx-runtime"] },
  server: {
    // Application HTTP keeps the native host's method and hostname policy.
    cors: false,
    allowedHosts: true,
    middlewareMode: true,
    hmr: { server },
    fs: { allow: [project, packageDir, dirname(host)] },
    watch: { ignored: ["**/.snap/**", "**/target/**", "**/*.rs", "**/Cargo.toml", "**/Cargo.lock"] },
  },
});
await new Promise<void>((done, fail) => {
  server.once("error", fail);
  server.listen(Number(listen.port || "80"), listen.hostname.replace(/^\[|\]$/g, ""), done);
});
let closing = false;
// Snap sends only complete accepted generations. Vite owns the browser channel.
const control = createInterface({ input: process.stdin });
control.on("line", (line) => {
  const update = JSON.parse(line) as { wasm: string; bindings: string; generation: string };
  wasm = update.wasm;
  bindings = update.bindings;
  generation = update.generation;
  vite.moduleGraph.invalidateAll();
  vite.ws.send({ type: "full-reload" });
});
async function close() {
  if (closing) return;
  closing = true;
  control.close();
  await vite.close();
  server.closeAllConnections();
  await new Promise<void>((done) => server.close(() => done()));
  process.exit(0);
}
process.on("SIGTERM", () => void close());
process.on("SIGINT", () => void close());
