import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { resolve } from "node:path";
import type { IdentityClient } from "../sdk/identity.contract";
import type { Snapshot } from "../../apps/authy/client";
import { deadline } from "./server";

export async function nativeIdentity(baseUrl: string): Promise<IdentityClient> {
  const build = await (await fetch(`${baseUrl}/__snap/build`)).json();
  const child = spawn(resolve(import.meta.dirname, "../../target/debug/examples/authy-sdk"), [], { env: { ...process.env, SNAP_BASE_URL: baseUrl, SNAP_BUILD: build.build }, stdio: ["pipe", "pipe", "pipe"] });
  let snapshot: Snapshot | undefined;
  let closed = false, sequence = 0, logs = "";
  const pending = new Map<number, { resolve(value: unknown): void; reject(error: unknown): void }>();
  const lines = createInterface({ input: child.stdout });
  child.stderr.on("data", chunk => { logs += chunk; });
  const exited = new Promise<void>(resolve => child.once("exit", () => { for (const call of pending.values()) call.reject(new Error(`SDK exited: ${logs}`)); pending.clear(); resolve(); }));
  child.on("error", error => { for (const call of pending.values()) call.reject(error); pending.clear(); });
  lines.on("line", line => {
    const value = JSON.parse(line);
    if (value.snapshot) { snapshot = value.snapshot; return; }
    const call = pending.get(value.id); if (!call) return;
    pending.delete(value.id);
    if (value.ok) call.resolve(value.value); else call.reject(value.error);
  });
  const command = (key: string, payload?: unknown) => {
    if (closed) return Promise.reject(new Error("Client is closed"));
    const id = ++sequence;
    return deadline(new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); child.stdin.write(`${JSON.stringify({ id, key, ...(payload === undefined ? {} : { payload }) })}\n`); }), 10_000);
  };
  try {
    const started = Date.now();
    while (!snapshot) { if (child.exitCode !== null || Date.now() - started > 10_000) throw new Error(`SDK startup failed: ${logs}`); await new Promise(done => setTimeout(done, 10)); }
    return {
      command,
      snapshot: async () => snapshot!,
      async close() {
        if (closed) return;
        try { await command("close"); } finally { closed = true; child.stdin.end(); try { await deadline(exited, 2_000); } catch { child.kill("SIGKILL"); await exited; } lines.close(); }
      },
    };
  } catch (error) { child.kill("SIGKILL"); await exited; throw error; }
}
