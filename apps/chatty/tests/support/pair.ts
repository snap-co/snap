import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { host as authyHost } from "../../../authy/tests/support/upgraded-host";

export async function pair(options: { root?: string; dev?: boolean; provider?: { key: string; search: string; endpoint?: string; model?: string } } = {}) {
  const root = options.root ?? resolve(import.meta.dir, "../../../..");
  const directory = await mkdtemp("/tmp/opencode/chatty-pair-");
  const reservation = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() });
  const base = `http://127.0.0.1:${reservation.port}`; reservation.stop(true);
  let authy: Awaited<ReturnType<typeof authyHost>> | undefined;
  let child: ReturnType<typeof Bun.spawn> | undefined;
  let logs = "";
  let modelCalls = 0;
  const waiting = new Set<() => void>();
  let restart: () => Promise<void> = async () => {};
  const model = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/release") { for (const release of waiting) release(); waiting.clear(); return new Response("released"); }
    if (path === "/stats") return Response.json({ calls: modelCalls });
    if (path === "/restart") { await restart(); return new Response("restarted"); }
    if (path !== "/responses") return new Response("not found", { status: 404 });
    modelCalls++;
    const input = await request.json() as { input: any[] };
    const prompt = [...input.input].reverse().find(item => item.role === "user")?.content ?? "";
    const answeredTool = input.input.at(-1)?.type === "function_call_output";
    const tool = prompt.includes("Write a note") && !answeredTool;
    const held = prompt.includes("Hold reply");
    const answer = answeredTool ? "Saved your note." : `Fixture answer: ${prompt}`;
    const output = tool ? [{ type: "function_call", call_id: "write-note", name: "write_file", arguments: JSON.stringify({ path: "notes.txt", content: "A private fixture note" }) }]
      : [{ type: "reasoning", encrypted_content: "fixture-opaque-provider-state", summary: [{ type: "summary_text", text: "Fixture provider summary" }] }, { type: "message", role: "assistant", phase: "final_answer", content: [{ type: "output_text", text: answer }] }];
    const encoder = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({ async start(controller) {
      const event = (value: unknown) => controller.enqueue(encoder.encode(`data: ${JSON.stringify(value)}\n\n`));
      try {
        if (held) {
          event({ type: "response.output_text.delta", delta: "Partial reply. ".repeat(80) });
          await new Promise<void>(resolve => waiting.add(resolve));
        }
        event({ type: "response.completed", response: { status: "completed", output, usage: { input_tokens: 12, output_tokens: 8, output_tokens_details: { reasoning_tokens: 2 } } } });
        controller.close();
      } catch { /* A stopped host may close its fixture stream. */ }
    } });
    return new Response(stream, { headers: { "content-type": "text/event-stream" } });
  } });
  const fixtureURL = `http://127.0.0.1:${model.port}`;
  async function stop() { if (child && child.exitCode === null) { child.kill("SIGTERM"); await child.exited; } }
  async function start() {
    logs = "";
    child = Bun.spawn(options.dev ? [`${root}/target/debug/snap`, "dev", `${root}/apps/chatty`] : [`${root}/target/debug/chatty`], { cwd: root, env: { ...process.env,
      SNAP_DATABASE: `${directory}/chatty.sqlite`, SNAP_WEB_DIR: `${root}/apps/chatty/.snap/web`,
      CHATTY_ADDR: new URL(base).host, SNAP_ORIGIN: base, AUTHY_ORIGIN: authy!.base,
      CHATTY_WEB_ADDR: new URL(base).host,
      CHATTY_CLIENT_SECRET: authy!.clientSecret, OPENCODE_API_KEY: options.provider?.key ?? "fixture-model-key", EXA_API_KEY: options.provider?.search ?? "",
      CHATTY_MODEL_ENDPOINT: options.provider ? options.provider.endpoint ?? "https://opencode.ai/zen/go/v1/responses" : `${fixtureURL}/responses`,
      CHATTY_MODEL: options.provider?.model ?? "muse-spark-1.3-contributor", CHATTY_FILES: `${directory}/files`,
    }, stdout: "pipe", stderr: "pipe" });
    const running = child;
    for (const stream of [running.stdout, running.stderr]) void (async () => { for await (const bytes of stream as ReadableStream<Uint8Array>) logs += new TextDecoder().decode(bytes); })();
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline && running.exitCode === null) {
      try { if (logs.includes(options.dev ? "Chatty dev http" : "Chatty http") && (await fetch(`${base}${options.dev ? "/api/session" : "/health"}`)).ok) return; } catch {}
      await Bun.sleep(20);
    }
    throw new Error(`Chatty failed to start:\n${logs}`);
  }
  async function close() { for (const release of waiting) release(); waiting.clear(); await stop(); await authy?.close(); model.stop(true); await rm(directory, { recursive: true, force: true }); }
  try {
    authy = await authyHost(base);
    const migrate = Bun.spawn([`${root}/target/debug/chatty`, "--migrate"], { env: { ...process.env, SNAP_DATABASE: `${directory}/chatty.sqlite` }, stdout: "ignore", stderr: "inherit" });
    if (await migrate.exited !== 0) throw new Error("Chatty migration failed");
    await start(); restart = async () => { await stop(); await start(); };
  } catch (error) { await close(); throw error; }
  return { base, fixtureURL, authy: authy!, directory, restart, stop, close, get logs() { return logs; }, get child() { return child!; } };
}
