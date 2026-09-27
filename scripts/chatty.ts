import { mkdir, readFile, appendFile } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";

const root = resolve(import.meta.dir, ".."); process.chdir(root);
const args = process.argv.slice(2);
if (args.some(arg => arg !== "--migrate")) throw new Error("usage: bun scripts/chatty.ts [--migrate]");
const configured: Record<string,string> = {};
try {
  for (const line of (await readFile(".snap/chatty.env", "utf8")).split("\n")) {
    const match = /^\s*(CHATTY_[A-Z_]+|OPENCODE_API_KEY|EXA_API_KEY|AUTHY_WEB_ADDR)=(.*)$/.exec(line);
    if (!match) continue;
    const value = match[2].trim(); configured[match[1]] = value.startsWith('"') ? JSON.parse(value) : value;
  }
} catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
const env = { ...configured, ...process.env } as Record<string,string>;
if (!env.CHATTY_CLIENT_SECRET && args.includes("--migrate")) {
  env.CHATTY_CLIENT_SECRET = randomBytes(32).toString("base64url");
  await mkdir(".snap", { recursive: true });
  await appendFile(".snap/chatty.env", `\nCHATTY_CLIENT_SECRET=${env.CHATTY_CLIENT_SECRET}\n`, { mode: 0o600 });
}
if (!env.CHATTY_CLIENT_SECRET || env.CHATTY_CLIENT_SECRET.length < 32) throw new Error("Set CHATTY_CLIENT_SECRET to at least 32 bytes, or explicitly initialize with --migrate");
env.AUTHY_WEB_ADDR ??= "127.0.0.1:3846"; env.CHATTY_WEB_ADDR ??= "127.0.0.1:3850";
env.AUTHY_ORIGIN = `http://${env.AUTHY_WEB_ADDR}`; env.CHATTY_ORIGIN = `http://${env.CHATTY_WEB_ADDR}`;
const children = new Set<ReturnType<typeof Bun.spawn>>();
let stopping = false;
async function stop() {
  if (stopping) return; stopping = true;
  await Promise.all([...children].map(async child => { if (child.exitCode === null) { child.kill("SIGTERM"); await child.exited; } }));
}
for (const [signal,code] of [["SIGINT",130],["SIGTERM",143]] as const) process.on(signal, () => void stop().then(() => process.exit(code)));
function spawn(command: string[], application: "authy" | "chatty") {
  if (stopping) throw new Error("Pair stopped");
  const child = Bun.spawn(command, { cwd: root, env: { ...env, SNAP_DATABASE: resolve(`apps/${application}/.snap/${application}-store.sqlite`) }, stdout: "inherit", stderr: "inherit" });
  children.add(child); void child.exited.then(() => children.delete(child)); return child;
}
try {
  if (args.includes("--migrate")) {
    for (const application of ["authy", "chatty"] as const) {
      if (await spawn(["./bin/snap", "build", `apps/${application}`], application).exited) throw new Error(`${application} build failed`);
      if (await spawn([`./dist/${application}/${application}`, "--migrate"], application).exited) throw new Error(`${application} migration failed`);
    }
    console.log("Authy and Chatty explicitly migrated. Run bun scripts/chatty.ts to develop.");
  } else {
    console.log(`Authy ${env.AUTHY_ORIGIN}; Chatty ${env.CHATTY_ORIGIN}`);
    const authy = spawn(["./bin/snap", "dev", "apps/authy"], "authy");
    const chatty = spawn(["./bin/snap", "dev", "apps/chatty"], "chatty");
    process.exitCode = await Promise.race([authy.exited, chatty.exited]);
  }
} finally { await stop(); }
