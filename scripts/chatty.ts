// Local native pair. Secrets are loaded only in this parent and passed through
// each child's environment, never command arguments or browser assets.
import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, readFile, appendFile, chmod } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";
const root = resolve(import.meta.dirname, "..");
const secretFile = resolve(root, ".snap/chatty.env");
await mkdir(resolve(root, ".snap"), { recursive: true });
const source = await readFile(secretFile, "utf8").catch(e => { if (e.code === "ENOENT") return ""; throw e; });
const secrets = Object.fromEntries(source.split(/\r?\n/).flatMap(line => { const match = line.match(/^([A-Z_][A-Z0-9_]*)=(.*)$/); if (!match) return []; let value = match[2].trim(); if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) value = value.slice(1, -1); return [[match[1], value]]; }));
if (!secrets.CHATTY_CLIENT_SECRET) {
  secrets.CHATTY_CLIENT_SECRET = randomBytes(32).toString("base64url");
  await appendFile(secretFile, `\nCHATTY_CLIENT_SECRET=${secrets.CHATTY_CLIENT_SECRET}\n`, { mode: 0o600 });
}
await chmod(secretFile, 0o600);
const host = process.env.CHATTY_HOST ?? "127.0.0.1";
const bind = process.env.CHATTY_BIND ?? "127.0.0.1";
const authyPort = process.env.AUTHY_PORT ?? "3846";
const chattyPort = process.env.CHATTY_PORT ?? "3850";
const authyOrigin = process.env.AUTHY_ORIGIN ?? `http://${host}:${authyPort}`;
const chattyOrigin = process.env.CHATTY_ORIGIN ?? `http://${host}:${chattyPort}`;
if (!process.argv.includes("--no-build")) {
  for (const cmd of [["./bin/snap", "build", "apps/authy"], ["cargo", "build", "-p", "chatty-native"], ["bun", "scripts/build-chatty.ts"]]) {
    const child = spawn(cmd[0], cmd.slice(1), { cwd: root, stdio: "inherit" });
    if (await new Promise<number | null>(done => child.once("exit", done))) throw new Error("Build failed");
  }
}
const children: ChildProcess[] = [];
let stopping = false;
function stop() { if (stopping) return; stopping = true; for (const child of children) child.kill("SIGTERM"); }
process.on("SIGINT", stop); process.on("SIGTERM", stop);
function start(binary: string, env: NodeJS.ProcessEnv) {
  const child = spawn(resolve(root, binary), [], { cwd: root, env: { ...process.env, ...env }, stdio: ["ignore", "inherit", "inherit"] });
  children.push(child); child.once("error", error => { console.error(error.message); stop(); });
  child.once("exit", code => { if (!stopping && code) process.exitCode = code; stop(); }); return child;
}
start("target/debug/authy", { SNAP_ADDR: `${bind}:${authyPort}`, SNAP_ORIGIN: authyOrigin, CHATTY_ORIGIN: chattyOrigin, CHATTY_CLIENT_SECRET: secrets.CHATTY_CLIENT_SECRET, SNAP_DATABASE: resolve(root, "apps/authy/.snap/authy.sqlite"), SNAP_WEB_DIR: resolve(root, "apps/authy/.snap/build/debug/web") });
start("target/debug/chatty", { SNAP_ADDR: `${bind}:${chattyPort}`, SNAP_ORIGIN: chattyOrigin, AUTHY_ORIGIN: authyOrigin, CHATTY_CLIENT_SECRET: secrets.CHATTY_CLIENT_SECRET, OPENCODE_API_KEY: secrets.OPENCODE_API_KEY ?? process.env.OPENCODE_API_KEY, EXA_API_KEY: secrets.EXA_API_KEY ?? process.env.EXA_API_KEY, SNAP_DATABASE: resolve(root, "apps/chatty/.snap/chatty.sqlite"), CHATTY_FILES: resolve(root, "apps/chatty/.snap/files"), SNAP_WEB_DIR: resolve(root, "apps/chatty/.snap/web") });
console.log(`Authy: ${authyOrigin}\nChatty: ${chattyOrigin}`);
await Promise.all(children.map(child => new Promise<void>(done => child.once("exit", () => done()))));
