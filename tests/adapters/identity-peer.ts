import { resolve } from "node:path";
import { authyServer } from "./authy";

function gate() {
  let release!: () => void;
  const wait = new Promise<void>(done => { release = done; });
  return { wait, release };
}

/** Real Authy authority behind a controlled HTTP carrier. Faults only change the
 * response or its delivery; SDK assertions never inspect the controller/store. */
export async function identityPeer() {
  const backend = await authyServer();
  const root = resolve(import.meta.dirname, "../..");
  const transpiler = new Bun.Transpiler({ loader: "ts", target: "browser" });
  const modules: Record<string, string> = {
    "/apps/authy/client.ts": "apps/authy/client.ts",
    "/clients/typescript/src": "clients/typescript/src/index.ts",
    "/apps/authy/.snap/bindings/authy_wasm.js": "apps/authy/.snap/bindings/authy_wasm.js",
    "/snap_client_wasm_bg.wasm": "apps/authy/.snap/bindings/authy_wasm_bg.wasm",
  };
  let hold: { path: string; arrived: ReturnType<typeof gate>; proceed: ReturnType<typeof gate> } | undefined;
  let malformed: string | undefined;
  let stale = false;
  const counts = new Map<string, number>();
  const peer = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const url = new URL(request.url);
    const file = modules[url.pathname];
    if (file) {
      if (file.endsWith(".ts")) return new Response(transpiler.transformSync(await Bun.file(resolve(root, file)).text()), { headers: { "content-type": "text/javascript" } });
      return new Response(Bun.file(resolve(root, file)), { headers: { "content-type": file.endsWith(".js") ? "text/javascript" : "application/wasm" } });
    }
    // These failure contracts assert HTTP recovery before Message readiness. A
    // deliberately unavailable socket cannot supply an alternative recovery path.
    if (url.pathname === "/_transport/ws") return new Response(null, { status: 503 });
    counts.set(url.pathname, (counts.get(url.pathname) ?? 0) + 1);
    try {
      const held = hold?.path === url.pathname ? hold : undefined;
      if (held) { held.arrived.release(); await held.proceed.wait; }
      const headers = new Headers(request.headers);
      headers.delete("host");
      if (headers.has("origin")) headers.set("origin", backend.baseUrl);
      if (stale && url.pathname !== "/__snap/build") headers.set("x-snap-build", "outdated-build");
      const response = await fetch(`${backend.baseUrl}${url.pathname}${url.search}`, { method: request.method, headers, body: request.method === "POST" ? await request.text() : undefined });
      if (malformed !== undefined && url.pathname === "/account/create") {
        const headers = new Headers(response.headers);
        headers.delete("content-length");
        await response.arrayBuffer();
        return new Response(malformed, { status: response.status, headers });
      }
      return response;
    } catch { return new Response(null, { status: 502 }); }
  } });
  return {
    baseUrl: `http://127.0.0.1:${peer.port}`,
    count: (path: string) => counts.get(path) ?? 0,
    corrupt(body: string) { malformed = body; },
    changeBuild() { stale = true; },
    hold(path: string) {
      hold = { path, arrived: gate(), proceed: gate() };
      const current = hold;
      return { arrived: current.arrived.wait, release: () => { current.proceed.release(); if (hold === current) hold = undefined; } };
    },
    async close() { hold?.proceed.release(); await peer.stop(true); await backend.close(); },
  };
}
