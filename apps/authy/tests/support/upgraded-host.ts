import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";

export async function host(relyingPartyOrigin = "http://127.0.0.1:3850", overrides: Record<string, string> = {}) {
  const root = resolve(import.meta.dir, "../../../..");
  const directory = await mkdtemp("/tmp/opencode/authy-host-");
  const binary = resolve(root, "target/debug/authy");
  const database = `${directory}/authy.sqlite`;
  const clientSecret = "fixture-client-secret-with-at-least-32-bytes";
  let base = "";
  let child: ReturnType<typeof Bun.spawn> | undefined;
  let logs = "";
  const env = { ...process.env, SNAP_DATABASE: database, SNAP_WEB_DIR: resolve(root, "apps/authy/.snap/web"),
    CHATTY_ORIGIN: relyingPartyOrigin, CHATTY_CLIENT_SECRET: clientSecret, ...overrides };
  async function start() {
    child = Bun.spawn([binary], { cwd: root, env: { ...env, AUTHY_ADDR: base ? new URL(base).host : "127.0.0.1:0" }, stdout: "pipe", stderr: "pipe" });
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
    const migrate = Bun.spawn([binary, "--migrate"], { cwd: root, env, stdout: "pipe", stderr: "pipe" });
    if (await migrate.exited !== 0) throw new Error(await new Response(migrate.stderr).text());
    await start();
  } catch (error) { await stop(); await rm(directory, { recursive: true, force: true }); throw error; }
  return { get base() { return base; }, database, clientSecret,
    async restart() { await stop(); logs = ""; await start(); },
    async close() { await stop(); await rm(directory, { recursive: true, force: true }); },
  };
}
