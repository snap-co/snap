// The official client owns local service discovery/authentication. This process
// carries API calls and event invalidations, never an agent loop.
import { OpenCode } from "@opencode/client";
import { Service } from "@opencode/client/service";

const endpoint = await Service.ensure({ command: [process.env.FACTORIO_OPENCODE ?? "opencode", "serve", "--service"] });
const headers = Service.headers(endpoint);
const input = await Bun.stdin.json() as { method?: string; path?: string; body?: unknown; watch?: string; description?: string };
async function request(path: string, method = "GET", body?: unknown) {
  const response = await fetch(new URL(path, endpoint.url), {
    method, headers: { ...headers, "content-type": "application/json" },
    body: body == null ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30000),
  });
  if (method === "DELETE" && response.status === 404) return null;
  if (!response.ok) throw new Error(`OpenCode returned ${response.status}`);
  return response.status === 204 ? null : await response.json();
}
type Message = { id: string; type: string; text?: string; metadata?: { factorio_initial?: boolean }; error?: { message?: string }; content?: { type: string; text?: string; name?: string; state?: { status?: string } }[] };
async function snapshot() {
  const path = `/api/session/${input.watch}`;
  const [messages, forms, permissions, session] = await Promise.all([
    request(`${path}/message?limit=100&order=desc`), request(`${path}/form`), request(`${path}/permission`), request(path),
  ]);
  return {
    messages: (messages.data as Message[]).toReversed().flatMap<Record<string, unknown>>(m => m.type === "user"
      ? [{ id: m.id, role: "user", text: m.metadata?.factorio_initial ? input.description : m.text }]
      : m.type === "assistant" ? [{ id: m.id, role: "assistant", parts: m.content?.flatMap<Record<string, unknown>>(c => c.type === "text" ? [{ type: "text", text: c.text }] : c.type === "tool" ? [{ type: "tool", name: c.name, status: c.state?.status }] : []), error: m.error?.message }] : []),
    forms: forms.data, permissions: permissions.data, outcome: session.data.outcome,
  };
}
if (input.watch) {
  const client = OpenCode.make({ baseUrl: endpoint.url, headers });
  let dirty = false, running = false;
  async function flush() {
    if (running) return;
    running = true;
    try {
      while (dirty) {
        dirty = false;
        console.log(JSON.stringify(await snapshot()));
        await Bun.sleep(200);
      }
    } catch { process.exit(1); }
    finally { running = false; }
  }
  for await (const event of client.event.subscribe()) {
    if (event.type === "server.connected" || JSON.stringify(event).includes(JSON.stringify(input.watch))) { dirty = true; void flush(); }
  }
  throw new Error("OpenCode event connection closed");
} else {
  console.log(JSON.stringify(await request(input.path!, input.method, input.body)));
}
