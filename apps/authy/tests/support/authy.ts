import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { startServer } from "../../../../tests/adapters/server";

/** A consumer-owned database and process; restart preserves the public address. */
export async function authyServer(dev = false, setup?: (database: string) => Promise<void>, config: { origin?: string; cached?: boolean } = {}) {
  const root = resolve(import.meta.dirname, "../../../..");
  const directory = await mkdtemp("/tmp/opencode/authy-");
  const options = {
    executable: dev ? undefined : resolve(root, config.cached ? "target/debug/examples/cached-authy" : "apps/authy/.snap/build/debug/authy"),
    dev, project: dev ? resolve(root, "apps/authy") : undefined,
    env: { SNAP_DATABASE: resolve(directory, "passport.sqlite"), ...(config.origin ? { SNAP_ORIGIN: config.origin } : {}) },
  };
  try {
    await setup?.(options.env.SNAP_DATABASE);
    let server = await startServer(options);
    const baseUrl = server.baseUrl;
    return {
      baseUrl,
      async restart(whileStopped?: () => Promise<void>) { await server.close(); await whileStopped?.(); server = await startServer({ ...options, address: new URL(baseUrl).host }); },
      async close() { try { await server.close(); } finally { await rm(directory, { recursive: true, force: true }); } },
    };
  } catch (error) { await rm(directory, { recursive: true, force: true }); throw error; }
}
