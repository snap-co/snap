// The selected TypeScript SDK is an independent consumer of Rust's HTTP and WS.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import { authyServer } from "./support/authy";
import { Socket } from "./support/socket";

const reference = process.env.SNAP_REFERENCE ?? join(homedir(), "code/bod/snap");
const resolve = createRequire(join(reference, "package.json")).resolve;
const [{ Effect, Layer, ManagedRuntime }, { Identity, Uuid }, { Passport, Transport }, { BrowserTransport }, { Account }] = await Promise.all([
  import(resolve("effect")), import(resolve("@snap/core")), import(resolve("@snap/engine")), import(resolve("@snap/browser/transport")), import(join(reference, "apps/authy/account.ts")),
]);
const server = await authyServer();
const build = await (await fetch(`${server.baseUrl}/__snap/build`)).json();
const originalFetch = globalThis.fetch;
const OriginalWebSocket = globalThis.WebSocket;
let cookie = "";
// Bun has browser Fetch/WebSocket but no browser cookie jar. This adapter only
// supplies cookie retention and Origin; the reference owns all Snap framing.
globalThis.fetch = (async (input: RequestInfo | URL, options?: RequestInit) => {
  const request = new Request(input, options);
  if (cookie) request.headers.set("cookie", cookie);
  const response = await originalFetch(request);
  const next = response.headers.get("set-cookie");
  if (next) cookie = next.split(";")[0];
  return response;
}) as typeof fetch;
globalThis.WebSocket = class extends Socket {
  constructor(url: string | URL) { super(url, { headers: { Origin: server.baseUrl, Cookie: cookie } }); }
} as typeof WebSocket;
const runtimes: any[] = [];
const start = () => {
  const runtime = ManagedRuntime.make(
    Layer.merge(Passport.Client({ passkey: Effect.succeed({}) }).layer, Account.Client.layer).pipe(
      Layer.provide(Transport.Client().layer),
      Layer.provide(BrowserTransport.Client.layer({ baseUrl: server.baseUrl, build })),
      Layer.provide(Uuid.layer(Uuid.default())),
    ),
  );
  runtimes.push(runtime);
  return runtime;
};
try {
  let runtime = start();
   const call = (work: (client: any) => any) => runtime.runPromise(Effect.gen(function* () { return yield* work(yield* Identity.Client); }));
  assert.equal((await call(c => c.fetch())).identityId, null);
  await runtime.runPromise(Effect.gen(function* () { const account = yield* Account.Service; yield* account.create({ email: "reference@example.test", password: "reference password" }); }));
  const identity = (await call(c => c.refresh())).identityId;
  assert.equal(typeof identity, "string");
  assert.equal((await call(c => c.credentials())).credentials[0].label, "reference@example.test");
  assert.equal((await call(c => c.sessions())).sessions[0].current, true);
  await call(c => c.release({ scope: "current" }));
  await runtime.dispose();
  runtime = start();
  assert.equal((await call(c => c.fetch())).identityId, null);
  await call(c => c.password.acquire({ kind: "user", email: "REFERENCE@example.test", password: "reference password" }));
  assert.equal((await call(c => c.refresh())).identityId, identity);
  assert.equal((await call(c => c.sessions())).sessions.length, 1);
  console.log("TypeScript Authy SDK → Rust password/session HTTP and WebSocket: PASS");
} finally {
  try { for (const runtime of runtimes) await runtime.dispose(); }
  finally { globalThis.fetch = originalFetch; globalThis.WebSocket = OriginalWebSocket; await server.close(); }
}
