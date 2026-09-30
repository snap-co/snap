import { mkdtemp, mkdir, rm, chmod, cp, symlink } from "node:fs/promises";
import { resolve } from "node:path";
import { host } from "../apps/authy/tests/support/upgraded-host";
import { openCodeFixture } from "../apps/factorio/tests/opencode-fixture";
import { deployment } from "../tests/support/deployment";
import { cliAuthorityFixture } from "../apps/factorio/tests/oauth-fixture";
const root = resolve(process.env.FACTORIO_SOURCE_ROOT ?? resolve(import.meta.dir, "..")); process.chdir(root);
if (process.argv.includes("--dev") && !process.env.FACTORIO_SOURCE_ROOT) {
  const copy = await mkdtemp(resolve(process.env.TMPDIR ?? "/tmp/opencode", "factorio-dev-source-"));
  try {
    for (const path of ["Cargo.toml","Cargo.lock","package.json","tsconfig.json","scripts","crates","platforms","kits","tools/cli","apps","tests"])
      await cp(`${root}/${path}`,`${copy}/${path}`,{recursive:true,filter:path=>!/(^|\/)(\.snap|\.deployment|node_modules|target|build|dist)(\/|$)/.test(path)});
    for (const path of ["node_modules",".tools","target"]) await symlink(`${root}/${path}`,`${copy}/${path}`);
    const rust=(await Bun.file(`${root}/mise.toml`).text()).match(/rust = "([^"]+)"/)![1];
    const child=Bun.spawn(["bun",`${root}/scripts/test-factorio.ts`],{env:{...process.env,MISE_RUST_VERSION:rust,FACTORIO_SOURCE_ROOT:copy,FACTORIO_TEST_DEV:"1"},stdout:"inherit",stderr:"inherit"});
    process.exitCode=await child.exited;
  } finally { await rm(copy,{recursive:true,force:true}); }
  process.exit(process.exitCode??0);
}
async function run(argv: string[], env = process.env, cwd = root) { const p = Bun.spawn(argv, { env, cwd, stdout: "inherit", stderr: "inherit" }); if (await p.exited !== 0) throw new Error(`Failed: ${argv.join(" ")}`); }
await run(["mise", "exec", "--", "cargo", "build", "-p", "snap-cli", "-p", "authy-native", "-p", "factorio-native"]);
await run([`${root}/target/debug/snap`, "build", "--project", "apps/authy", "--web-only", "--output", `${root}/apps/authy/dist/development/web`]);
await run([`${root}/target/debug/snap`, "build", "--project", "apps/factorio", "--web-only", "--output", `${root}/apps/factorio/dist/development/web`]);
await run(["bunx", "tsc", "-p", "apps/factorio/web/tsconfig.json"]);
const directory = await mkdtemp(resolve(process.env.TMPDIR ?? "/tmp/opencode", "factorio-journey-"));
let authy: Awaited<ReturnType<typeof host>> | undefined, child: ReturnType<typeof Bun.spawn> | undefined;
let logs = "";
let setup: Awaited<ReturnType<typeof deployment>>;
const reserve = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
const base = `http://127.0.0.1:${reserve.port}`; reserve.stop(true);
const resourcePort = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
const firstPort = resourcePort.port!; resourcePort.stop(true);
const tcpPort = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
const tcp = `127.0.0.1:${tcpPort.port}`; tcpPort.stop(true);
const secret = "factorio-fixture-client-secret-32-characters";
const repository = `${directory}/repo`, resources = `${directory}/resources`;
async function stop() { if (child?.exitCode === null) { child.kill("SIGTERM"); await child.exited; } }
async function start() {
  logs = "";
  child = Bun.spawn(process.env.FACTORIO_TEST_DEV ? [`${root}/target/debug/snap`,"dev",`${root}/apps/factorio`, "--config", setup.path] : [`${root}/target/debug/factorio`, "serve", "--config", setup.path], { cwd: root, env: { ...setup.env, PATH: `${directory}/bin:${process.env.PATH}`, FACTORIO_FIXTURE_API: `http://127.0.0.1:${control.port}`, FACTORIO_FIXTURE: directory }, stdout: "pipe", stderr: "pipe" });
  const running = child;
  for (const stream of [child.stdout, child.stderr]) void (async()=>{for await (const b of stream as ReadableStream<Uint8Array>) logs += new TextDecoder().decode(b);})();
  const until = Date.now()+60000;
  while (Date.now()<until && running.exitCode===null) { try { if (logs.includes(process.env.FACTORIO_TEST_DEV ? "Factorio dev http" : "Factorio http") && (await fetch(`${base}/api/session`)).ok) return; } catch {} await Bun.sleep(20); }
  throw new Error(`Factorio startup failed: ${logs}`);
}
const opencodeFixture = openCodeFixture();
const control = Bun.serve({hostname:"127.0.0.1",port:0, idleTimeout: 0, async fetch(request) { const fixture=await opencodeFixture(request);if(fixture)return fixture;if(authy){const authority=await cliAuthorityFixture(request,directory,authy,stop,start);if(authority)return authority;}const path=new URL(request.url).pathname;if(path==="/build-state")return Response.json({failed:logs.includes("Rebuild failed; previous generation retained"),restartRequired:logs.includes("Server arguments changed; restart snap dev"),generations:(logs.match(/generation ready/g)??[]).length});if(path!=="/restart")return new Response("missing",{status:404});await stop();await start();return new Response("restarted"); }});
try {
  // Disposable private CA, unrelated to operator trust or certificates.
  await run(["openssl","req","-x509","-newkey","ec","-pkeyopt","ec_paramgen_curve:P-256","-nodes","-days","2","-subj","/CN=Factorio fixture CA","-keyout",`${directory}/ca-key.pem`,"-out",`${directory}/ca.pem`,"-addext","basicConstraints=critical,CA:TRUE"]);
  await run(["openssl","req","-new","-newkey","ec","-pkeyopt","ec_paramgen_curve:P-256","-nodes","-subj","/CN=localhost","-keyout",`${directory}/server-key.pem`,"-out",`${directory}/server.csr`,"-addext","subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1","-addext","extendedKeyUsage=serverAuth","-addext","basicConstraints=critical,CA:FALSE"]);
  await run(["openssl","x509","-req","-in",`${directory}/server.csr`,"-CA",`${directory}/ca.pem`,"-CAkey",`${directory}/ca-key.pem`,"-CAcreateserial","-days","2","-copy_extensions","copy","-out",`${directory}/server.pem`]);
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
else if(path==='/api/session'){if(!body.id||!body.location?.directory)process.exit(2);sessions[body.id]={...body,directory:body.location.directory};console.log(JSON.stringify({data:sessions[body.id]}));}
else if(path.endsWith('/move')){if(!sessions[id]||!body.directory)process.exit(2);sessions[id].directory=body.directory;}
else if(path.endsWith('/model')){if(!sessions[id]||!body.model?.providerID||!body.model?.id)process.exit(2);sessions[id].model=body.model;}
else if(path.endsWith('/prompt')){if(!sessions[id]||!body.text)process.exit(2);console.log(JSON.stringify({data:{id:body.id}}));}
else process.exit(2);
await fs.writeFile(file,JSON.stringify(sessions));
`);await chmod(`${directory}/bin/opencode`,0o755);
  await Bun.write(`${directory}/setup.sh`, '#!/bin/sh\nif [ "$FACTORIO_SESSION" = fail ] && [ ! -f "$FACTORIO_DATA/permit" ]; then echo "fixture setup failure" >&2; exit 1; fi\nprintf "%s" "$PORT" > "$FACTORIO_DATA/port"\n');
  const repositoryConfig = {repository,mainline:"main",modules:{a:"crates/a",b:"crates/b"},resources,first_port:firstPort,setup:["/bin/sh",`${directory}/setup.sh`],teardown:[]};
  authy=await host(base,{FACTORIO_ORIGIN:base,FACTORIO_CLIENT_SECRET:secret});
  setup = await deployment(directory, { host: { mode: "development", listen: new URL(base).host, origin: base, data_dir: directory, database: "factorio.sqlite", web_dir: `${root}/apps/factorio/dist/development/web` },
    app: { tcp: { listen: tcp, cert_file: "../../server.pem", key_file: "../../server-key.pem", ca_file: "../../ca.pem" }, repository: repositoryConfig, oauth: { issuer: authy.base, client_id: "factorio", client_secret_ref: "oauth.client_secret" }, tools: { bun: process.execPath, opencode: `${directory}/bin/opencode`, bridge: `${root}/apps/factorio/tests/opencode-bridge.ts` } } }, { oauth: { client_secret: secret } });
  await run([`${root}/target/debug/factorio`,"serve", "--migrate", "--config", setup.path],setup.env);await start();
  await run(["bunx","playwright","test","--config","apps/factorio/tests/playwright.config.ts"],{...process.env,FACTORIO_TEST_URL:base,FACTORIO_ADDR:tcp,FACTORIO_CA_FILE:`${directory}/ca.pem`,FACTORIO_SERVER_NAME:"localhost",FACTORIO_FIXTURE:directory,FACTORIO_FIXTURE_URL:`http://127.0.0.1:${control.port}`,FACTORIO_FIXTURE_DIR:directory});
} catch (error) { console.error(logs); throw error; }
finally {await stop();await authy?.close();control.stop(true);await rm(directory,{recursive:true,force:true});}
