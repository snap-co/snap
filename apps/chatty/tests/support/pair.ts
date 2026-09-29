import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { host as authyHost } from "../../../authy/tests/support/upgraded-host";
import { devHosts, originsFor } from "../../../../scripts/dev-network";
import { deployment } from "../../../../tests/support/deployment";

export async function pair(options: { root?: string; dev?: boolean } = {}) {
  const root = options.root ?? resolve(import.meta.dir, "../../../..");
  const directory = await mkdtemp("/tmp/opencode/chatty-pair-");
  const reservation = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
  const base = `http://127.0.0.1:${reservation.port}`; reservation.stop(true);
  let authy: Awaited<ReturnType<typeof authyHost>> | undefined;
  let child: ReturnType<typeof Bun.spawn> | undefined;
  let logs = "";
  let setup: Awaited<ReturnType<typeof deployment>>;
  let restart: () => Promise<void> = async () => {};
  const control = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/restart") { await restart(); return new Response("restarted"); }
    return new Response("not found", { status: 404 });
  } });
  const fixtureURL = `http://127.0.0.1:${control.port}`;
  async function stop() { if (child && child.exitCode === null) { child.kill("SIGTERM"); await child.exited; } }
  async function start() {
    logs = "";
    child = Bun.spawn(options.dev ? [`${root}/target/debug/snap`, "dev", `${root}/apps/chatty`, "--config", setup.path] : [`${root}/target/debug/chatty`, "--config", setup.path], { cwd: root, env: setup.env, stdout: "pipe", stderr: "pipe" });
    const running = child;
    for (const stream of [running.stdout, running.stderr]) void (async () => { for await (const bytes of stream as ReadableStream<Uint8Array>) logs += new TextDecoder().decode(bytes); })();
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline && running.exitCode === null) {
      try { if (logs.includes(options.dev ? "Chatty dev http" : "Chatty http") && (await fetch(`${base}${options.dev ? "/api/session" : "/health"}`)).ok) return; } catch {}
      await Bun.sleep(20);
    }
    throw new Error(`Chatty failed to start:\n${logs}`);
  }
  async function close() { await stop(); await authy?.close(); control.stop(true); await rm(directory, { recursive: true, force: true }); }
  try {
    authy = await authyHost(base, options.dev ? {
      SNAP_DEV_MODE: "1", SNAP_DEV_CLIENT_ORIGINS: JSON.stringify({ chatty: originsFor(await devHosts(), new URL(base).port) }),
    } : {});
    setup = await deployment(directory, { host: { mode: "development", listen: new URL(base).host, origin: base, data_dir: directory, database: "chatty.sqlite", web_dir: `${root}/apps/chatty/dist/development/web` },
      app: { oauth: { issuer: authy.base, client_id: "chatty", client_secret_ref: "oauth.client_secret" } },
      ...(options.dev ? { dev: { listen: `0.0.0.0:${new URL(base).port}` } } : {}) }, { oauth: { client_secret: authy.clientSecret } });
    const migrate = Bun.spawn([`${root}/target/debug/chatty`, "--migrate", "--config", setup.path], { env: setup.env, stdout: "ignore", stderr: "inherit" });
    if (await migrate.exited !== 0) throw new Error("Chatty migration failed");
    await start(); restart = async () => { await stop(); await start(); };
  } catch (error) { await close(); throw error; }
  return { base, fixtureURL, authy: authy!, directory, restart, stop, close, get logs() { return logs; }, get child() { return child!; } };
}
