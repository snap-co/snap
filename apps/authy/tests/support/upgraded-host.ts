import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { deployment } from "../../../../tests/support/deployment";

export async function host(relyingPartyOrigin = "http://127.0.0.1:3850", overrides: Record<string, string> = {}) {
  const root = resolve(import.meta.dir, "../../../..");
  const directory = await mkdtemp("/tmp/opencode/authy-host-");
  const binary = resolve(root, "target/debug/authy");
  const database = `${directory}/authy.sqlite`;
  const clientSecret = "fixture-client-secret-with-at-least-32-bytes";
  let base = "";
  let child: ReturnType<typeof Bun.spawn> | undefined;
  let logs = "";
  const clients = [{ id: "chatty", name: "Chatty", origin: relyingPartyOrigin, client_secret_ref: "clients.chatty" }];
  if (overrides.FACTORIO_CLIENT_SECRET) clients.push({ id: "factorio", name: "Factorio", origin: overrides.FACTORIO_ORIGIN, client_secret_ref: "clients.factorio" });
  let setup: Awaited<ReturnType<typeof deployment>>;
  async function configure() {
    setup = await deployment(directory, { host: { mode: "development", listen: base ? new URL(base).host : "127.0.0.1:0", data_dir: directory, database: "authy.sqlite", web_dir: resolve(root, "apps/authy/dist/development/web"),
      ...(overrides.SNAP_DEV_CLIENT_ORIGINS ? { dev_client_origins: JSON.parse(overrides.SNAP_DEV_CLIENT_ORIGINS) } : {}) },
      app: { clients, auto_approve_domain: overrides.AUTHY_AUTO_APPROVE_DOMAIN ?? "snapco.dev", ...(overrides.AUTHY_APP_DOMAIN ? { app_domain: overrides.AUTHY_APP_DOMAIN } : {}) } },
      { clients: { chatty: clientSecret, ...(overrides.FACTORIO_CLIENT_SECRET ? { factorio: overrides.FACTORIO_CLIENT_SECRET } : {}) } });
  }
  async function start() {
    await configure();
    child = Bun.spawn([binary, "--config", setup.path], { cwd: root, env: setup.env, stdout: "pipe", stderr: "pipe" });
    const running = child;
    let address = "";
    for (const stream of [running.stdout, running.stderr]) void (async () => {
      for await (const chunk of stream as ReadableStream<Uint8Array>) {
        const text = new TextDecoder().decode(chunk); logs += text;
        address ||= /Authy (http:\/\/[^\s]+)/.exec(logs)?.[1] ?? "";
      }
    })();
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline && running.exitCode === null) {
      if (address) {
        try { if ((await fetch(`${address}/health`)).ok) { base = address; return; } } catch {}
      }
      await Bun.sleep(20);
    }
    throw new Error(`Authy startup failed:\n${logs}`);
  }
  async function stop() {
    if (child && child.exitCode === null) { child.kill("SIGTERM"); await child.exited; }
  }
  try {
    await configure();
    const migrate = Bun.spawn([binary, "--migrate", "--config", setup.path], { cwd: root, env: setup.env, stdout: "pipe", stderr: "pipe" });
    if (await migrate.exited !== 0) throw new Error(await new Response(migrate.stderr).text());
    await start();
  } catch (error) { await stop(); await rm(directory, { recursive: true, force: true }); throw error; }
  return { get base() { return base; }, database, clientSecret,
    async restart(beforeStart?: () => Promise<void>) { await stop(); logs = ""; await beforeStart?.(); await start(); },
    async close() { await stop(); await rm(directory, { recursive: true, force: true }); },
  };
}
