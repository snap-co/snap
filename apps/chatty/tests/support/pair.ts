import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { host as authyHost } from "../../../authy/tests/support/upgraded-host";
import { devHosts, originsFor } from "../../../../scripts/dev-network";

export async function pair(options: { root?: string; dev?: boolean } = {}) {
  const root = options.root ?? resolve(import.meta.dir, "../../../..");
  const directory = await mkdtemp("/tmp/opencode/chatty-pair-");
  const reservation = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
  const base = `http://127.0.0.1:${reservation.port}`; reservation.stop(true);
  let authy: Awaited<ReturnType<typeof authyHost>> | undefined;
  let child: ReturnType<typeof Bun.spawn> | undefined;
  let logs = "";
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
    child = Bun.spawn(options.dev ? [`${root}/target/debug/snap`, "dev", `${root}/apps/chatty`] : [`${root}/target/debug/chatty`], { cwd: root, env: { ...process.env,
      SNAP_DATABASE: `${directory}/chatty.sqlite`, SNAP_WEB_DIR: `${root}/apps/chatty/.snap/web`,
      CHATTY_ADDR: new URL(base).host, SNAP_ORIGIN: base, AUTHY_ORIGIN: authy!.base,
      CHATTY_WEB_ADDR: options.dev ? `0.0.0.0:${new URL(base).port}` : new URL(base).host,
      CHATTY_CLIENT_SECRET: authy!.clientSecret,
    }, stdout: "pipe", stderr: "pipe" });
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
    const migrate = Bun.spawn([`${root}/target/debug/chatty`, "--migrate"], { env: { ...process.env, SNAP_DATABASE: `${directory}/chatty.sqlite` }, stdout: "ignore", stderr: "inherit" });
    if (await migrate.exited !== 0) throw new Error("Chatty migration failed");
    await start(); restart = async () => { await stop(); await start(); };
  } catch (error) { await close(); throw error; }
  return { base, fixtureURL, authy: authy!, directory, restart, stop, close, get logs() { return logs; }, get child() { return child!; } };
}
