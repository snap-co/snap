import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer } from "node:net";
import { createServer as httpServer } from "node:http";
import { startServer } from "./server";
import { workersServer } from "./workers";

export const clientOrigin = "http://127.0.0.1:3850";
export const clientSecret = "oidc-fixture-only-secret-32-characters";
export async function freePort() {
  const server = createServer();
  await new Promise<void>((done, fail) => { server.once("error", fail); server.listen(0, "127.0.0.1", done); });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No fixture address");
  await new Promise<void>((done, fail) => server.close(error => error ? fail(error) : done()));
  return address.port;
}
export async function oidcServer(host: "native" | "workers", relyingPartyOrigin = clientOrigin) {
  if (host === "workers") return workersServer("authy", { CHATTY_ORIGIN: relyingPartyOrigin, CHATTY_CLIENT_SECRET: clientSecret });
  const directory = await mkdtemp("/tmp/opencode/oidc-");
  const address = `127.0.0.1:${await freePort()}`;
  const options = { executable: resolve("target/debug/authy"), address, webDirectory: resolve("apps/authy/.snap/build/debug/web"), env: { SNAP_DATABASE: resolve(directory, "authy.sqlite"), SNAP_ORIGIN: `http://${address}`, CHATTY_ORIGIN: relyingPartyOrigin, CHATTY_CLIENT_SECRET: clientSecret } };
  try {
    let server = await startServer(options);
    return { baseUrl: server.baseUrl, logs: server.logs, restart: async () => { await server.close(); server = await startServer(options); }, close: async () => { try { await server.close(); } finally { await rm(directory, { recursive: true, force: true }); } } };
  } catch (e) { await rm(directory, { recursive: true, force: true }); throw e; }
}
/** A real cross-origin destination keeps browser redirect/CSP behavior observable. */
export async function oidcBrowserServer(host: "native" | "workers") {
  const rp = httpServer((_request, response) => { response.writeHead(200, { "content-type": "text/html" }); response.end("<h1>Relying party reached</h1>"); });
  await new Promise<void>((done, fail) => { rp.once("error", fail); rp.listen(0, "127.0.0.1", done); });
  const address = rp.address();
  if (!address || typeof address === "string") throw new Error("No RP fixture address");
  const origin = `http://127.0.0.1:${address.port}`;
  const closeRp = () => new Promise<void>((done, fail) => { rp.closeAllConnections(); rp.close(error => error ? fail(error) : done()); });
  try {
    const authy = await oidcServer(host, origin);
    return { ...authy, clientOrigin: origin, close: async () => { try { await authy.close(); } finally { await closeRp(); } } };
  } catch (e) { await closeRp(); throw e; }
}
