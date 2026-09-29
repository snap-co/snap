import { resolve } from "node:path";

const root = resolve(import.meta.dir, ".."); process.chdir(root);
const args = process.argv.slice(2);
if (args.some(arg => arg !== "--migrate")) throw new Error("usage: bun scripts/chatty.ts [--migrate]");
const children = new Set<ReturnType<typeof Bun.spawn>>();
let stopping = false;
async function stop() {
  if (stopping) return; stopping = true;
  await Promise.all([...children].map(async child => { if (child.exitCode === null) { child.kill("SIGTERM"); await child.exited; } }));
}
for (const [signal,code] of [["SIGINT",130],["SIGTERM",143]] as const) process.on(signal, () => void stop().then(() => process.exit(code)));
function spawn(command: string[], application: "authy" | "chatty") {
  if (stopping) throw new Error("Pair stopped");
  const child = Bun.spawn(command, { cwd: root, env: process.env, stdout: "inherit", stderr: "inherit" });
  children.add(child); void child.exited.then(() => children.delete(child)); return child;
}
try {
  if (args.includes("--migrate")) {
    for (const application of ["authy", "chatty"] as const) {
      if (await spawn(["./bin/snap", "build", "--project", `apps/${application}`], application).exited) throw new Error(`${application} build failed`);
      if (await spawn([`./apps/${application}/dist/development/server`, "--migrate"], application).exited) throw new Error(`${application} migration failed`);
    }
    console.log("Authy and Chatty explicitly migrated. Run bun scripts/chatty.ts to develop.");
  } else {
    console.log("Using each app's .deployment/development configuration and encrypted secrets.");
    const authy = spawn(["./bin/snap", "dev", "apps/authy"], "authy");
    const chatty = spawn(["./bin/snap", "dev", "apps/chatty"], "chatty");
    process.exitCode = await Promise.race([authy.exited, chatty.exited]);
  }
} finally { await stop(); }
