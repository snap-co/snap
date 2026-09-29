import { copyFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer, type ProxyOptions, type ViteDevServer } from "vite";
import react from "@vitejs/plugin-react";
import { development, generation } from "../platforms/config/development";

// App-owned host composition; snap owns the process group and signal forwarding.
const root = resolve(import.meta.dir, "..");
process.chdir(root);
const input = await development("testy", root);
const publicURL = new URL(`http://${input.config.dev?.listen ?? input.config.host.listen}`);
if (!["127.0.0.1", "[::1]"].includes(publicURL.hostname))
  throw new Error("Testy development requires a loopback address");
await mkdir("apps/testy/.snap", { recursive: true });
const session = await mkdtemp(resolve("apps/testy/.snap/dev-"));
const children = new Set<ReturnType<typeof Bun.spawn>>();
let vite: ViteDevServer | undefined;
let stopping = false;
let backend = "";
const proxyTarget = new URL("http://127.0.0.1:1");
let serial = 0;
let revision = 0;
let building = false;
let timer: ReturnType<typeof setTimeout> | undefined;

function spawn(command: string[], env = process.env, capture = false) {
  if (stopping) throw new Error("Development stopped");
  const child = Bun.spawn(command, {
    env, stdout: capture ? "pipe" : "inherit", stderr: "inherit",
  });
  children.add(child);
  void child.exited.then(() => children.delete(child));
  return child;
}
async function run(command: string[], env = process.env) {
  if (await spawn(command, env).exited !== 0)
    throw new Error(`Failed: ${command.join(" ")}`);
}
async function stop(child: ReturnType<typeof Bun.spawn>) {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  const timeout = setTimeout(() => child.kill("SIGKILL"), 1500);
  await child.exited;
  clearTimeout(timeout);
}
async function shutdown(code: number) {
  if (stopping) return;
  stopping = true;
  clearTimeout(timer);
  await vite?.close();
  await Promise.all([...children].map(stop));
  await rm(session, { recursive: true, force: true });
  process.exit(code);
}
process.on("SIGINT", () => void shutdown(130));
process.on("SIGTERM", () => void shutdown(143));

async function build() {
  const directory = `${session}/${++serial}`;
  await mkdir(directory);
  await run([process.env.SNAP_CLI!, "build", "--project", "apps/testy", "--web-only", "--output", directory]);
  await run(["mise", "exec", "--", "cargo", "build", "-p", "testy-local",
    "--no-default-features", "--features", "web", "--bin", "testy-web"]);
  await copyFile("target/debug/testy-web", `${directory}/testy-web`);
  return directory;
}

async function launch(directory: string) {
  const config = await generation(directory, input, { listen: backend ? new URL(backend).host : "127.0.0.1:0", origin: backend || undefined, web_dir: directory });
  const child = spawn([`${directory}/testy-web`, "--config", config], { ...process.env, SNAP_MASTER_KEY: input.key }, true);
  let address = "";
  void (async () => {
    let output = "";
    for await (const chunk of child.stdout as ReadableStream<Uint8Array>) {
      const text = new TextDecoder().decode(chunk);
      process.stdout.write(text);
      output = (output + text).slice(-4096);
      address ||= /Testy (http:\/\/[^\s]+)/.exec(output)?.[1] ?? "";
    }
  })();
  try {
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline && child.exitCode === null) {
      if (address) {
        const response = await fetch(`${address}/__dev`, { signal: AbortSignal.timeout(500) });
        if (response.ok) {
          backend = address;
          proxyTarget.href = address;
          return child;
        }
      }
      await Bun.sleep(30);
    }
    throw new Error("Testy host did not become ready; check the explicitly migrated database");
  } catch (error) {
    await stop(child);
    throw error;
  }
}

try {
  // Watch before the initial build so edits made during compilation are not lost.
  // Vite owns frontend HMR; Rust changes publish a complete, private generation.
  let current = "";
  let running: Awaited<ReturnType<typeof launch>> | undefined;
  const proxy: ProxyOptions = {
    target: proxyTarget, ws: true, changeOrigin: true,
    bypass(request) {
      // Validate the public origin before translating it to the private host.
      if (request.headers.host !== publicURL.host ||
          (request.headers.origin && request.headers.origin !== publicURL.origin)) return false;
      if (request.headers.origin) request.headers.origin = backend;
    },
  };
  vite = await createServer({
    configFile: false,
    root: resolve("apps/testy/web"),
    publicDir: false,
    plugins: [react(), {
      name: "testy-bindings",
      enforce: "pre",
      resolveId(id) {
        if (id.endsWith("/.snap/web/bindings/testy_wasm.js"))
          return `${current}/bindings/testy_wasm.js`;
      },
      transformIndexHtml(html) {
        return html.replace('<link rel="stylesheet" href="/app.css" />', "")
          .replace('src="/app.js"', 'src="/app.tsx"');
      },
      configureServer(server) {
        server.middlewares.use(async (req, res, next) => {
          if (req.url?.split("?")[0] !== "/bindings/testy_wasm_bg.wasm") return next();
          res.setHeader("Content-Type", "application/wasm");
          res.setHeader("Cache-Control", "no-store");
          res.end(Buffer.from(await Bun.file(`${current}/bindings/testy_wasm_bg.wasm`).arrayBuffer()));
        });
      },
    }],
    server: {
      host: publicURL.hostname.replace(/[\[\]]/g, ""),
      port: Number(publicURL.port || 80), strictPort: true,
      fs: { allow: [resolve("apps/testy/web"), session, resolve("node_modules")] },
      proxy: { "^/(transport|__dev)(/|$)": proxy },
      watch: { ignored: ["**/target/**", "**/.snap/**", "**/dist/**", "**/.git/**"] },
    },
  });
  vite.watcher.add([resolve("crates"), resolve("platforms"), resolve("apps/testy"),
    resolve("Cargo.toml"), resolve("Cargo.lock")]);
  const rebuild = async () => {
    if (building || stopping) return;
    building = true;
    try {
      let again = true;
      while (again && !stopping) {
        const started = revision;
        try {
          const candidate = await build();
          if (revision !== started) continue;
          if (running) await stop(running);
          try {
            running = await launch(candidate);
          } catch (error) {
            if (!current) throw error;
            running = await launch(current);
            throw error;
          }
          current = candidate;
          // Bindings and native code are now from the same successful generation.
          vite!.environments.client.moduleGraph.invalidateAll();
          vite!.ws.send({ type: "full-reload" });
          console.log(`Testy generation ready: ${serial}`);
        } catch (error) {
          if (!running || running.exitCode !== null) throw error;
          console.error("Rebuild failed; previous generation retained:", error);
        }
        again = revision !== started;
      }
    } finally { building = false; }
  };
  vite.watcher.on("all", (_event, path) => {
    if (!/\.(rs|toml|lock)$/.test(path) || path.includes("/.snap/") || path.includes("/dist/")) return;
    revision++;
    clearTimeout(timer);
    timer = setTimeout(() => void rebuild().catch(error => {
      console.error(error); void shutdown(1);
    }), 150);
  });
  await rebuild();
  await vite.listen();
  const address = vite.httpServer!.address();
  if (address && typeof address !== "string") publicURL.port = String(address.port);
  console.log(`Testy dev ${publicURL.origin} (frontend HMR; Rust/Wasm rebuild and reload)`);
  // Surface unexpected host termination rather than leave a dead proxy running.
  setInterval(() => {
    if (!building && running?.exitCode !== null) {
      console.error("Testy host exited"); void shutdown(1);
    }
  }, 250).unref();
} catch (error) {
  console.error(error);
  await shutdown(1);
}
