import { mkdtemp, mkdir, rm, chmod, cp, symlink } from "node:fs/promises";
import { resolve } from "node:path";
import { host } from "../apps/authy/tests/support/upgraded-host";
import { openCodeFixture } from "../apps/factorio/tests/opencode-fixture";
const root = resolve(process.env.FACTORIO_SOURCE_ROOT ?? resolve(import.meta.dir, "..")); process.chdir(root);
if (process.argv.includes("--dev") && !process.env.FACTORIO_SOURCE_ROOT) {
  const copy = await mkdtemp("/tmp/opencode/factorio-dev-source-");
  try {
    for (const path of ["Cargo.toml","Cargo.lock","package.json","tsconfig.json","scripts","crates","platforms","kits","tools/cli","apps","tests/properties"])
      await cp(`${root}/${path}`,`${copy}/${path}`,{recursive:true,filter:path=>!/(^|\/)(\.snap|node_modules|target|build)(\/|$)/.test(path)});
    for (const path of ["node_modules",".tools","target"]) await symlink(`${root}/${path}`,`${copy}/${path}`);
    const rust=(await Bun.file(`${root}/mise.toml`).text()).match(/rust = "([^"]+)"/)![1];
    const child=Bun.spawn(["bun",`${root}/scripts/test-factorio.ts`],{env:{...process.env,MISE_RUST_VERSION:rust,FACTORIO_SOURCE_ROOT:copy,FACTORIO_TEST_DEV:"1"},stdout:"inherit",stderr:"inherit"});
    process.exitCode=await child.exited;
  } finally { await rm(copy,{recursive:true,force:true}); }
  process.exit(process.exitCode??0);
}
async function run(argv: string[], env = process.env, cwd = root) { const p = Bun.spawn(argv, { env, cwd, stdout: "inherit", stderr: "inherit" }); if (await p.exited !== 0) throw new Error(`Failed: ${argv.join(" ")}`); }
await run(["mise", "exec", "--", "cargo", "build", "-p", "authy-native", "-p", "factorio-native"]);
await run(["bun", "scripts/build-authy.ts"]); await run(["bun", "scripts/build-factorio.ts"]);
await run(["bunx", "tsc", "-p", "apps/factorio/web/tsconfig.json"]);
const directory = await mkdtemp("/tmp/opencode/factorio-journey-");
let authy: Awaited<ReturnType<typeof host>> | undefined, child: ReturnType<typeof Bun.spawn> | undefined;
let logs = "";
const reserve = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
const base = `http://127.0.0.1:${reserve.port}`; reserve.stop(true);
const resourcePort = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
const firstPort = resourcePort.port!; resourcePort.stop(true);
const secret = "factorio-fixture-client-secret-32-characters";
const repository = `${directory}/repo`, resources = `${directory}/resources`;
async function stop() { if (child?.exitCode === null) { child.kill("SIGTERM"); await child.exited; } }
async function start() {
  logs = "";
  child = Bun.spawn(process.env.FACTORIO_TEST_DEV ? [`${root}/target/debug/snap`,"dev",`${root}/apps/factorio`] : [`${root}/target/debug/factorio`], { cwd: root, env: { ...process.env, PATH: `${directory}/bin:${process.env.PATH}`, FACTORIO_BUN: process.execPath, FACTORIO_OPENCODE_BRIDGE: `${root}/apps/factorio/tests/opencode-bridge.ts`, FACTORIO_FIXTURE_API: `http://127.0.0.1:${control.port}`, FACTORIO_FIXTURE: directory, FACTORIO_CONFIG: `${directory}/config.json`, SNAP_DATABASE: `${directory}/factorio.sqlite`, FACTORIO_WEB_ADDR: new URL(base).host, FACTORIO_ADDR: new URL(base).host, SNAP_ORIGIN: base, AUTHY_ORIGIN: authy!.base, FACTORIO_CLIENT_SECRET: secret, SNAP_WEB_DIR: `${root}/apps/factorio/.snap/web` }, stdout: "pipe", stderr: "pipe" });
  const running = child;
  for (const stream of [child.stdout, child.stderr]) void (async()=>{for await (const b of stream as ReadableStream<Uint8Array>) logs += new TextDecoder().decode(b);})();
  const until = Date.now()+60000;
  while (Date.now()<until && running.exitCode===null) { try { if (logs.includes(process.env.FACTORIO_TEST_DEV ? "Factorio dev http" : "Factorio http") && (await fetch(`${base}/api/session`)).ok) return; } catch {} await Bun.sleep(20); }
  throw new Error(`Factorio startup failed: ${logs}`);
}
const opencodeFixture = openCodeFixture();
const control = Bun.serve({hostname:"127.0.0.1",port:0, idleTimeout: 0, async fetch(request) { const fixture=await opencodeFixture(request);if(fixture)return fixture;const path=new URL(request.url).pathname;if(path==="/build-state")return Response.json({failed:logs.includes("Rebuild failed; previous generation retained"),generations:(logs.match(/generation ready/g)??[]).length});if(path!=="/restart")return new Response("missing",{status:404});await stop();await start();return new Response("restarted"); }});
try {
  await mkdir(`${repository}/crates/a`,{recursive:true});await mkdir(`${repository}/crates/b`,{recursive:true});await mkdir(`${directory}/bin`);
  await Bun.write(`${repository}/crates/a/file`,"base a");await Bun.write(`${repository}/crates/b/file`,"base b");
  await run(["git","init","-b","main"],process.env,repository);await run(["git","add","."],process.env,repository);await run(["git","-c","user.name=Fixture","-c","user.email=fixture@localhost","commit","-m","fixture baseline"],process.env,repository);
  // Explicit OpenCode V2 contract fixture. No real conversations or global service changes.
  await Bun.write(`${directory}/bin/opencode`, `#!/usr/bin/env bun
const fs = await import('node:fs/promises');
const [api,method,path,...rest]=process.argv.slice(2);
if(api==='--session'){console.log(JSON.stringify({resumed:method}));process.exit(0);}
if(api!=='api')process.exit(2);
const file=process.env.FACTORIO_FIXTURE+'/conversations.json';
let sessions={};try{sessions=JSON.parse(await fs.readFile(file,'utf8'));}catch{}
const body=rest[0]==='--data'?JSON.parse(rest[1]):{};
const id=path.split('/')[3];
if(method==='get'){if(!sessions[id])process.exit(1);console.log(JSON.stringify({data:sessions[id]}));}
else if(path==='/api/session'){if(!body.id||!body.location?.directory)process.exit(2);sessions[body.id]={id:body.id,directory:body.location.directory};console.log(JSON.stringify({data:sessions[body.id]}));}
else if(path.endsWith('/move')){if(!sessions[id]||!body.directory)process.exit(2);sessions[id].directory=body.directory;}
else if(path.endsWith('/model')){if(!sessions[id]||!body.model?.providerID||!body.model?.id)process.exit(2);sessions[id].model=body.model;}
else process.exit(2);
await fs.writeFile(file,JSON.stringify(sessions));
`);await chmod(`${directory}/bin/opencode`,0o755);
  await Bun.write(`${directory}/setup.sh`, '#!/bin/sh\nif [ "$FACTORIO_SESSION" = fail ] && [ ! -f "$FACTORIO_DATA/permit" ]; then echo "fixture setup failure" >&2; exit 1; fi\nprintf "%s" "$PORT" > "$FACTORIO_DATA/port"\n');
  await Bun.write(`${directory}/config.json`,JSON.stringify({repository,mainline:"main",modules:{a:"crates/a",b:"crates/b"},resources,first_port:firstPort,setup:["/bin/sh",`${directory}/setup.sh`],teardown:[]}));
  authy=await host(base,{FACTORIO_ORIGIN:base,FACTORIO_CLIENT_SECRET:secret});
  await run([`${root}/target/debug/factorio`,"--migrate"],{...process.env,SNAP_DATABASE:`${directory}/factorio.sqlite`});await start();
  await run(["bunx","playwright","test","--config","apps/factorio/tests/playwright.config.ts"],{...process.env,FACTORIO_TEST_URL:base,FACTORIO_FIXTURE_URL:`http://127.0.0.1:${control.port}`,FACTORIO_FIXTURE_DIR:directory});
} finally {await stop();await authy?.close();control.stop(true);await rm(directory,{recursive:true,force:true});}
