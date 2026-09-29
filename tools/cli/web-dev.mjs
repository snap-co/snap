// Vite adapter only. Rust owns configuration, builds and backend lifetime.
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { createRequire } from "node:module";

const initial = JSON.parse(await readFile(process.argv[2], "utf8"));
// The embedded helper runs from a private directory, not the app's node_modules.
const require = createRequire(join(initial.project, "package.json"));
const { createServer } = await import(require.resolve("vite"));
const { default: react } = await import(require.resolve("@vitejs/plugin-react"));
let state = initial;
const target = new URL(state.backend);
const originFor = headers => state.origins.find(origin => new URL(origin).host === headers.host &&
  (headers.origin === undefined || headers.origin === origin));
const emit = message => process.stdout.write(JSON.stringify(message) + "\n");
const forward = (outgoing, request) => {
  const external = originFor(request.headers);
  if (!external) { outgoing.destroy(); return; }
  outgoing.setHeader("host", new URL(state.origin).host);
  if (request.headers.origin) outgoing.setHeader("origin", state.origin);
  outgoing.setHeader("x-snap-dev-origin", external);
  for (const name of ["forwarded", "x-forwarded-host", "x-forwarded-proto", "x-forwarded-for"])
    outgoing.removeHeader(name);
};
const logger = {
  hasWarned: false,
  info: message => console.error(message),
  warn(message) { this.hasWarned = true; console.error(message); },
  warnOnce(message) { this.warn(message); },
  error: message => console.error(message),
  clearScreen() {}, hasErrorLogged() { return false; },
};
const server = await createServer({
  configFile: false, root: join(state.project, "web"), publicDir: false, customLogger: logger,
  plugins: [react(), {
    name: "snap-development", enforce: "pre",
    resolveId(id) { if (id === "@snap/wasm") return join(state.generation, "web/bindings", `${state.library}.js`); },
    transformIndexHtml: { order: "pre", handler(html) {
      // Use the canonical module URL. Serving TSX under /app.js runs refresh's
      // self-import a second time and creates competing React roots.
      return html.replace(/<link\b[^>]*href=["']\/app\.css["'][^>]*>/g, "")
        .replace(/src=(["'])\/app\.js\1/g, 'src="/app.tsx"');
    } },
    configureServer(vite) {
      vite.httpServer?.prependListener("upgrade", (req, socket) => {
        if (!originFor(req.headers)) { socket.write("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n"); socket.destroy(); }
      });
      vite.middlewares.use(async (req, res, next) => {
        if (!originFor(req.headers)) { res.statusCode = 403; res.end("Unrecognized development origin"); return; }
        const path = req.url?.split("?")[0];
        const files = {
          [`/bindings/${state.library}.js`]: ["text/javascript", `${state.library}.js`],
          [`/bindings/${state.library}_bg.wasm`]: ["application/wasm", `${state.library}_bg.wasm`],
        };
        if (!files[path]) return next();
        try {
          res.setHeader("Content-Type", files[path][0]); res.setHeader("Cache-Control", "no-store");
          res.end(await readFile(join(state.generation, "web/bindings", files[path][1])));
        } catch { res.statusCode = 503; res.end("Bindings unavailable"); }
      });
    },
  }],
  server: {
    host: state.listenHost, port: state.listenPort, strictPort: true,
    allowedHosts: state.origins.map(origin => new URL(origin).hostname), cors: false,
    fs: { allow: [state.workspace, resolve(state.project, "../../node_modules"), state.session] },
    proxy: { "^/(api|identity|auth|oauth|\\.well-known|transport|__dev)(/|$)": {
      target, ws: true, changeOrigin: false,
      configure(proxy) { proxy.on("proxyReq", forward); proxy.on("proxyReqWs", forward); },
    } },
    watch: { ignored: ["**/target/**", "**/.snap/**", "**/dist/**", "**/.git/**", "**/.deployment/**"] },
  },
});
await server.listen();
emit({ event: "ready" });
const input = createInterface({ input: process.stdin });
for await (const line of input) {
  const message = JSON.parse(line);
  if (message.command === "publish") {
    state = message.state; target.href = state.backend;
    server.environments.client.moduleGraph.invalidateAll();
    server.ws.send({ type: "full-reload" });
    emit({ event: "published" });
  }
}
await server.close();
