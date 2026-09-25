import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer } from "node:net";
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
export async function oidcServer(host: "native" | "workers") {
  if (host === "workers") return workersServer("authy", { CHATTY_ORIGIN: clientOrigin, CHATTY_CLIENT_SECRET: clientSecret });
  const directory = await mkdtemp("/tmp/opencode/oidc-");
  const address = `127.0.0.1:${await freePort()}`;
  const options = { executable: resolve("target/debug/authy"), address, env: { SNAP_DATABASE: resolve(directory, "authy.sqlite"), SNAP_ORIGIN: `http://${address}`, CHATTY_ORIGIN: clientOrigin, CHATTY_CLIENT_SECRET: clientSecret } };
  try {
    let server = await startServer(options);
    return { baseUrl: server.baseUrl, logs: server.logs, restart: async () => { await server.close(); server = await startServer(options); }, close: async () => { try { await server.close(); } finally { await rm(directory, { recursive: true, force: true }); } } };
  } catch (e) { await rm(directory, { recursive: true, force: true }); throw e; }
}
