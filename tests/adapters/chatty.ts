import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { freePort, oidcServer } from "./oidc";
import { startServer } from "./server";
import { workersServer } from "./workers";
const root = resolve(import.meta.dirname, "../..");

export async function modelServer() {
  const requests: any[] = [];
  const releases: (() => void)[] = [];
  let active = 0, peak = 0;
  const server = createServer(async (req, res) => {
    const parts: Buffer[] = []; for await (const part of req) parts.push(Buffer.from(part));
    const body = JSON.parse(Buffer.concat(parts).toString()); requests.push(body);
    active++; peak = Math.max(peak, active); res.once("close", () => { active--; });
    res.writeHead(200, { "content-type": "text/event-stream" });
    const send = (value: unknown) => res.write(`data: ${JSON.stringify(value)}\n\n`);
    const prompt = body.input.findLast((item: any) => item.role === "user")?.content ?? "";
    const hasResult = body.input.at(-1)?.type === "function_call_output";
    if (prompt === "hold") { send({ type: "response.output_text.delta", delta: "Started" }); await new Promise<void>(done => { releases.push(done); }); }
    if (prompt === "broken") { res.end(); return; }
    const tool = prompt.startsWith("write:") ? { name: "write_file", arguments: JSON.stringify({ path: "notes/proof.txt", content: "private note" }) } : prompt.startsWith("read:") ? { name: "read_file", arguments: JSON.stringify({ path: prompt.slice(5) }) } : null;
    let output: any[];
    if (tool && !hasResult) output = [{ type: "reasoning", id: "rs_tool", encrypted_content: "opaque-tool", summary: [] }, { type: "function_call", id: "fc_test", call_id: `call_${requests.length}`, ...tool }];
    else {
      const answer = prompt === "hold" ? "Started and finished" : hasResult ? `Tool result: ${body.input.at(-1).output}` : `Reply to ${prompt}`;
      send({ type: "response.reasoning_summary_text.delta", delta: "A short supplied summary." });
      send({ type: "response.output_text.delta", delta: answer });
      output = [{ type: "reasoning", id: "rs_test", encrypted_content: "opaque-reasoning", summary: [{ type: "summary_text", text: "A short supplied summary." }] }, { type: "message", id: "msg_test", role: "assistant", status: "completed", phase: "final_answer", content: [{ type: "output_text", text: answer, annotations: [] }] }];
    }
    send({ type: "response.completed", response: { status: "completed", output, usage: { input_tokens: 20, output_tokens: 15, output_tokens_details: { reasoning_tokens: 5 } } } });
    if (prompt !== "terminal-kept-open") res.end();
  });
  await new Promise<void>((done, fail) => { server.once("error", fail); server.listen(0, "127.0.0.1", done); });
  const address = server.address(); if (!address || typeof address === "string") throw new Error("Missing model port");
  const release = () => { for (const done of releases.splice(0)) done(); };
  return { endpoint: `http://127.0.0.1:${address.port}/responses`, requests, active: () => active, peak: () => peak, release, close: async () => { release(); server.closeAllConnections(); await new Promise<void>(done => server.close(() => done())); } };
}
export async function chattyServer(options: { live?: boolean; env?: NodeJS.ProcessEnv; host?: "native" | "workers" } = {}) {
  const directory = await mkdtemp("/tmp/opencode/chatty-");
  const model = options.live ? null : await modelServer();
  const address = `127.0.0.1:${await freePort()}`;
  const origin = `http://${address}`;
  const authy = await oidcServer(options.host ?? "native", origin);
  const config = { executable: resolve(root, "target/debug/chatty"), address, env: { SNAP_DATABASE: resolve(directory, "chatty.sqlite"), SNAP_ORIGIN: origin, AUTHY_ORIGIN: authy.baseUrl, CHATTY_FILES: resolve(directory, "files"), CHATTY_CLIENT_SECRET: "oidc-fixture-only-secret-32-characters", OPENCODE_API_KEY: "fixture", ...options.env, ...(model ? { CHATTY_MODEL_ENDPOINT: model.endpoint } : {}) } };
  const start = async () => options.host === "workers"
    ? workersServer("chatty", { AUTHY_ORIGIN: authy.baseUrl, CHATTY_CLIENT_SECRET: config.env.CHATTY_CLIENT_SECRET, OPENCODE_API_KEY: config.env.OPENCODE_API_KEY, ...(model ? { CHATTY_MODEL_ENDPOINT: model.endpoint } : {}) }, Number(address.split(":")[1]), "workerName" in authy ? authy.workerName : undefined)
    : startServer({ ...config, webDirectory: resolve(root, "apps/chatty/.snap/web") });
  let app: Awaited<ReturnType<typeof start>>;
  try { app = await start(); } catch (e) { await authy.close(); await model?.close(); await rm(directory, { recursive: true, force: true }); throw e; }
  return { baseUrl: app.baseUrl, authy: authy.baseUrl, model, directory, logs: () => app.logs(), restart: async () => { if ("restart" in app) await app.restart(); else { await app.close(); app = await start(); } }, close: async () => { try { await app.close(); await authy.close(); await model?.close(); } finally { await rm(directory, { recursive: true, force: true }); } } };
}

export class BrowserSession {
  private cookies = new Map<string, string>();
  constructor(readonly base: string, readonly authy: string) {}
  csrf = "";
  async fetch(url: string, init: RequestInit = {}) {
    const headers = new Headers(init.headers); headers.set("cookie", [...this.cookies].map(([key, value]) => `${key}=${value}`).join("; "));
    const response = await fetch(new URL(url, this.base), { ...init, headers, redirect: "manual" });
    for (const line of response.headers.getSetCookie()) { const pair = line.split(";")[0]; const [key, ...value] = pair.split("="); this.cookies.set(key, value.join("=")); }
    return response;
  }
  async login(email: string, create = true) {
    let response = await this.fetch("/auth/login");
    if (response.status !== 303) throw new Error(`Login start: ${response.status} ${await response.text()}`);
    const authorize = response.headers.get("location")!;
    const registration = await this.fetch(`${this.authy}/${create ? "account/create" : "identity/password/acquire"}`, { method: "POST", headers: { "x-snap-build": "healthy-smoke", "content-type": "application/json", origin: this.authy }, body: JSON.stringify({ email, password: "chatty fixture password" }) });
    if (!(await registration.json()).payload.ok) throw new Error("Authy registration failed");
    response = await this.fetch(authorize);
    const html = await response.text(); const handle = html.match(/name=request value="([^"]+)"/)?.[1];
    if (!handle) throw new Error(`Missing consent: ${response.status} ${html}`);
    response = await this.fetch(`${this.authy}/oauth/authorize`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded", origin: this.authy }, body: new URLSearchParams({ request: handle, decision: "allow" }) });
    response = await this.fetch(response.headers.get("location")!);
    if (response.status !== 303) throw new Error(`Callback: ${response.status} ${await response.text()}`);
    const session = await (await this.fetch("/api/session")).json(); this.csrf = session.csrf;
    if (!session.identified) throw new Error("Chatty session missing"); return session;
  }
  async post(path: string, body: unknown) { return this.fetch(path, { method: "POST", headers: { "content-type": "application/json", origin: this.base, "x-chatty-csrf": this.csrf }, body: JSON.stringify(body) }); }
  async view(id: string) { return (await this.fetch(`/api/thread?id=${encodeURIComponent(id)}`)).json(); }
  async settled(id: string) {
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) { const view = await this.view(id); if (!view.thread?.active_turn) return view; await new Promise(done => setTimeout(done, 30)); }
    throw new Error("Turn did not settle");
  }
}
