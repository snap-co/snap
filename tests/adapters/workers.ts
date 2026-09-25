import { spawn } from "node:child_process";
import { mkdtemp, rm, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { resolve } from "node:path";

/** Real local workerd, with fixture-owned persistence and no remote bindings. */
export async function workersServer(application: "authy" | "healthy" | "contract" | "chatty", vars: Record<string, string> = {}, selectedPort?: number, authyService?: string) {
  const root = resolve(import.meta.dirname, "../..");
  const cwd = resolve(root, application === "contract" ? "tests/workers" : application === "healthy" ? "tests/fixtures/healthy/workers" : `apps/${application}/workers`);
  const directory = await mkdtemp("/tmp/opencode/snap-workers-");
  const workerName = `fixture-${application}-${crypto.randomUUID()}`;
  let configuration: string | undefined;
  if (authyService) {
    const config = JSON.parse(await readFile(resolve(cwd, "wrangler.jsonc"), "utf8"));
    config.main = resolve(cwd, config.main); config.build.cwd = cwd;
    config.assets.directory = resolve(cwd, config.assets.directory);
    config.services = [{ binding: "AUTHY", service: authyService }];
    configuration = resolve(directory, "wrangler.json");
    await writeFile(configuration, JSON.stringify(config));
  }
  const listener = createServer();
  await new Promise<void>((done, reject) => { listener.once("error", reject); listener.listen(0, "127.0.0.1", done); });
  const address = listener.address();
  if (!address || typeof address === "string") throw new Error("Missing fixture port");
  const port = selectedPort ?? address.port;
  await new Promise<void>((done, reject) => listener.close(error => error ? reject(error) : done()));
  const baseUrl = `http://127.0.0.1:${port}`;
  let process: ReturnType<typeof spawn>;
  let output = "";
  const start = async () => {
    process = spawn("node", [resolve(root, "node_modules/wrangler/bin/wrangler.js"), "dev", "--local", "--name", workerName, "--port", String(port), "--inspector-port", "0", "--persist-to", directory,
      ...(configuration ? ["--config", configuration] : []),
      ...(application !== "healthy" ? ["--var", `SNAP_ORIGIN:${baseUrl}`, "--var", "SNAP_BUILD:healthy-smoke"] : []),
      ...Object.entries(vars).flatMap(([key, value]) => ["--var", `${key}:${value}`]),
    ], { cwd, detached: true, env: { ...globalThis.process.env, WRANGLER_SEND_METRICS: "false", CI: "true" }, stdio: ["ignore", "pipe", "pipe"] });
    await new Promise<void>((done, reject) => {
      const timeout = setTimeout(() => reject(new Error(`Workers startup timed out:\n${output}`)), 90_000);
      const read = (chunk: Buffer) => { const text = String(chunk); output += text; if (text.includes("Ready on")) { clearTimeout(timeout); done(); } };
      process.stdout!.on("data", read); process.stderr!.on("data", read);
      process.once("error", error => { clearTimeout(timeout); reject(error); });
      process.once("exit", code => { clearTimeout(timeout); reject(new Error(`Workers exited ${code}:\n${output}`)); });
    });
  };
  const stop = async () => {
    if (!process?.pid || process.exitCode !== null || process.signalCode !== null) return;
    await new Promise<void>(done => {
      const timer = setTimeout(() => { try { globalThis.process.kill(-process.pid!, "SIGKILL"); } catch {} }, 5_000);
      process.once("exit", () => { clearTimeout(timer); done(); });
      try { globalThis.process.kill(-process.pid!, "SIGTERM"); } catch { clearTimeout(timer); done(); }
    });
  };
  try {
    await start();
    return {
      baseUrl,
      workerName,
      logs: () => output,
      restart: async (whileStopped?: () => Promise<void>) => { await stop(); await whileStopped?.(); await start(); },
      close: async () => { try { await stop(); } finally { await rm(directory, { recursive: true, force: true }); } },
    };
  } catch (error) { await stop(); await rm(directory, { recursive: true, force: true }); throw error; }
}
