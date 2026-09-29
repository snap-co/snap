import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer, type ViteDevServer } from "vite";
import react from "@vitejs/plugin-react";
import { clientOrigins, devHosts, originsFor, publicOrigin, requestOrigin } from "./dev-network";
import { development, generation } from "../platforms/config/development";

export async function dev(app: "authy" | "chatty" | "factorio", entry: "app" | "main", port: number) {
const title = app[0].toUpperCase() + app.slice(1);
const root = resolve(import.meta.dir, "..");
process.chdir(root);
const input = await development(app, root);
const listenURL = new URL(`http://${input.config.dev?.listen ?? input.config.host.listen}`);
const publicURL = publicOrigin(input.config.host.origin ?? `http://127.0.0.1:${listenURL.port || 80}`);
const behindTLS = publicURL.protocol === "https:";
if (behindTLS && !["127.0.0.1", "[::1]"].includes(listenURL.hostname)) {
  throw new Error("HTTPS public origins require a loopback dev.listen behind the TLS proxy");
}
const hosts = await devHosts();
hosts.push(publicURL.hostname);
let origins: string[] = [];
await mkdir(`apps/${app}/.snap`, { recursive: true });
const session = await mkdtemp(resolve(`apps/${app}/.snap/dev-`));
const children = new Set<ReturnType<typeof Bun.spawn>>();
const target = new URL("http://127.0.0.1:1");
let vite: ViteDevServer | undefined;
let stopping = false, building = false, revision = 0, serial = 0;
let current = "", backend = "";
let timer: ReturnType<typeof setTimeout> | undefined;
let running: ReturnType<typeof Bun.spawn> | undefined;
function spawn(command: string[], env = process.env, capture = false) {
  if (stopping) throw new Error("Development stopped");
  const child = Bun.spawn(command, { env, stdout: capture ? "pipe" : "inherit", stderr: "inherit" });
  children.add(child); void child.exited.then(() => children.delete(child)); return child;
}
async function run(command: string[], env = process.env) { if (await spawn(command, env).exited !== 0) throw new Error(`Failed: ${command.join(" ")}`); }
async function stop(child: ReturnType<typeof Bun.spawn>) {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM"); const timeout = setTimeout(() => child.kill("SIGKILL"), 1500);
  await child.exited; clearTimeout(timeout);
}
async function shutdown(code: number) {
  if (stopping) return; stopping = true; clearTimeout(timer);
  await vite?.close(); await Promise.all([...children].map(stop));
  await rm(session, { recursive: true, force: true }); process.exit(code);
}
process.on("SIGINT", () => void shutdown(130)); process.on("SIGTERM", () => void shutdown(143));
async function build() {
  const directory = `${session}/${++serial}`; await mkdir(directory);
  await run([process.env.SNAP_CLI!, "build", "--project", `apps/${app}`, "--output", directory]);
  return directory;
}
async function launch(directory: string) {
  const config = await generation(directory, input, {
    listen: backend ? new URL(backend).host : "127.0.0.1:0", origin: publicURL.origin,
    dev_origins: origins, web_dir: `${directory}/web`,
    dev_client_origins: app === "authy" ? clientOrigins(hosts, input.config.app.clients, input.config.app.app_domain) : {},
  });
  const child = spawn([`${directory}/server`, "--config", config], { ...process.env, SNAP_MASTER_KEY: input.key }, true);
  let address = "";
  void (async () => { let output = ""; for await (const chunk of child.stdout as ReadableStream<Uint8Array>) {
    const text = new TextDecoder().decode(chunk); process.stdout.write(text); output = (output + text).slice(-4096);
    address ||= new RegExp(`${title} (http://[^\\s]+)`).exec(output)?.[1] ?? "";
  } })();
  try {
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline && child.exitCode === null) {
      if (address && (await fetch(`${address}/health`, { signal: AbortSignal.timeout(500) })).ok) { backend = address; target.href = address; return child; }
      await Bun.sleep(30);
    }
    throw new Error(`${title} host did not become ready; explicitly migrate the configured database first`);
  } catch (error) { await stop(child); throw error; }
}
try {
  vite = await createServer({ configFile: false, root: resolve(`apps/${app}/web`), publicDir: false,
    plugins: [react(), { name: `${app}-bindings`, enforce: "pre",
      transformIndexHtml: { order: "pre", handler(html) { return html.replace(`<link rel="stylesheet" href="/${entry}.css" />`, "").replace(`src="/${entry}.js"`, `src="/${entry}.tsx"`); } },
      configureServer(server) {
        server.httpServer?.prependListener("upgrade", (req, socket) => {
          if (!requestOrigin(origins, req.headers)) { socket.write("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n"); socket.destroy(); }
        });
        server.middlewares.use(async (req, res, next) => {
        if (!requestOrigin(origins, req.headers)) { res.statusCode = 403; res.end("Unrecognized development origin"); return; }
        const path = req.url?.split("?")[0];
        if (path !== `/bindings/${app}_wasm.js` && path !== `/bindings/${app}_wasm_bg.wasm`) return next();
        if (!current) { res.statusCode = 503; res.end(); return; }
        res.setHeader("Content-Type", path.endsWith(".wasm") ? "application/wasm" : "text/javascript");
        res.setHeader("Cache-Control", "no-store"); res.end(Buffer.from(await Bun.file(`${current}/web${path}`).arrayBuffer()));
      }); },
    }],
    server: { host: listenURL.hostname.replace(/[\[\]]/g, ""), port: Number(listenURL.port || 80), strictPort: true,
      allowedHosts: [...hosts], cors: false,
      fs: { allow: [resolve(`apps/${app}/web`), session, resolve("node_modules")] },
      proxy: { "^/(api|identity|auth|oauth|\\.well-known|transport)(/|$)": { target, ws: true, changeOrigin: false,
        configure(proxy) {
          // The loopback host retains one canonical authority. Only this dev proxy
          // translates already-validated same-origin requests and supplies the
          // external origin used for OAuth callbacks. Never trust incoming forwarding headers.
          const forward = (outgoing: import("node:http").ClientRequest, request: import("node:http").IncomingMessage) => {
            const external = requestOrigin(origins, request.headers);
            if (!external) { outgoing.destroy(); return; }
            outgoing.setHeader("host", publicURL.host);
            if (request.headers.origin) outgoing.setHeader("origin", publicURL.origin);
            outgoing.setHeader("x-snap-dev-origin", external);
            for (const header of ["forwarded", "x-forwarded-host", "x-forwarded-proto", "x-forwarded-for"]) outgoing.removeHeader(header);
          };
          proxy.on("proxyReq", forward); proxy.on("proxyReqWs", forward);
        } } },
      watch: { ignored: ["**/target/**", "**/.snap/**", "**/dist/**", "**/.git/**"] } },
  });
  vite.watcher.add([resolve("crates"), resolve("platforms"), resolve(`apps/${app}`), resolve("Cargo.toml"), resolve("Cargo.lock")]);
  async function rebuild() {
    if (building || stopping) return; building = true;
    try {
      let again = true;
      while (again && !stopping) {
        const started = revision;
        try {
          const candidate = await build(); if (revision !== started) continue;
          if (running) await stop(running);
          try { running = await launch(candidate); }
          catch (error) { if (current) running = await launch(current); throw error; }
          current = candidate; vite!.environments.client.moduleGraph.invalidateAll(); vite!.ws.send({ type: "full-reload" });
          console.log(`${title} generation ready: ${serial}`);
        } catch (error) { if (!running || running.exitCode !== null) throw error; console.error("Rebuild failed; previous generation retained:", error); }
        again = revision !== started;
      }
    } finally { building = false; }
  }
  vite.watcher.on("all", (_event, path) => {
    if ((!/\.(rs|toml|lock)$/.test(path) && !(app === "authy" && path.endsWith("/auth-ui.tsx"))) || path.includes("/.snap/") || path.includes("/dist/")) return;
    revision++; clearTimeout(timer); timer = setTimeout(() => void rebuild().catch(error => { console.error(error); void shutdown(1); }), 150);
  });
  await vite.listen(); const address = vite.httpServer!.address();
  if (address && typeof address !== "string") {
    if (!input.config.host.origin) publicURL.port = String(address.port);
    origins = behindTLS ? [publicURL.origin] : [...new Set([...originsFor(hosts, address.port), publicURL.origin])];
  }
  console.log(`${title} dev origins: ${origins.join(", ")}`);
  await rebuild(); console.log(`${title} dev ${publicURL.origin} (frontend HMR; Rust/Wasm rebuild and reload)`);
  setInterval(() => { if (!building && running?.exitCode !== null) { console.error(`${title} host exited`); void shutdown(1); } }, 250).unref();
} catch (error) { console.error(error); await shutdown(1); }
}
