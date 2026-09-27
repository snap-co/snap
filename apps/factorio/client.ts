/** Shared browser/CLI carrier. Domain rules live in the Rust application. */
export type Ticket = { id: string; title: string; description: string; modules: string[]; status: "draft" | "ready" | "done" | "cancelled"; notes: string; parent: string | null; blockers: string[] };
export type Candidate = { commit: string; target: string; evidence: string; findings: { text: string; disposition: string }[]; approval: { human: string; at: number; commit: string } | null };
export type Session = { id: string; owner: string; prompt: string; tickets: string[]; modules: string[]; phase: string; base: string; branch: string; worktree: string; data: string; port: number; conversation: string; candidate: Candidate | null; integration: string | null; error: string };
export type Workspace = { config: { repository: string; mainline: string; modules: Record<string, string> }; tickets: Record<string, Ticket>; sessions: Record<string, Session> };
export type Identity = { identified: boolean; csrf?: string; owner?: string; human?: boolean };
export class Factorio {
  identity: Identity = { identified: false };
  constructor(readonly origin: string, private readonly token?: string, private readonly transport: typeof fetch = globalThis.fetch.bind(globalThis)) {}
  private async request<T>(path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = {};
    if (this.token) headers.authorization = `Bearer ${this.token}`;
    if (body !== undefined) { headers["content-type"] = "application/json"; if (!this.token) headers["x-snap-csrf"] = this.identity.csrf ?? ""; }
    const response = await this.transport(`${this.origin}${path}`, { method: body === undefined ? "GET" : "POST", headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: "same-origin" });
    const value = await response.json().catch(() => null);
    if (!response.ok) throw new Error(value?.error_description ?? `Request failed (${response.status})`);
    return value as T;
  }
  async identify() { return this.identity = await this.request<Identity>("/api/session"); }
  workspace() { return this.request<Workspace>("/api/workspace"); }
  command(command: Record<string, unknown>) { return this.request<Workspace>("/api/command", command); }
  approve(id: string, commit: string) { return this.request<Workspace>("/api/approve", { id, commit }); }
  agentToken() { return this.request<{ token: string }>("/api/token", {}); }
  logout() { return this.request<{ redirect: string }>("/auth/logout", {}); }
}
type Binding = { connect(id: string): string; receive(text: string): string; free(): void };
/** Browser-owned socket; the portable Document client validates and recovers state. */
export async function subscribe(client: Factorio, update: (workspace: Workspace | null, error?: string) => void) {
  const path = "/bindings/factorio_wasm.js";
  const module = await import(/* @vite-ignore */ path);
  await module.default({ module_or_path: "/bindings/factorio_wasm_bg.wasm" });
  let binding: Binding | undefined, socket: WebSocket | undefined, timer: ReturnType<typeof setTimeout> | undefined, closed = false;
  const id = crypto.randomUUID();
  async function connect() {
    try {
      const identity = await client.identify();
      if (closed) return;
      if (!identity.identified) { update(null, "Sign in to continue"); return; }
      binding?.free(); binding = new module.FactorioClient(identity.owner);
      const peer = socket = new WebSocket(`${client.origin.replace(/^http/, "ws")}/transport`);
      peer.onopen = () => peer.send(binding!.connect(id));
      peer.onmessage = event => { try { const result = JSON.parse(binding!.receive(String(event.data))); update(result.workspace); for (const frame of result.send) peer.send(frame); } catch (e) { update(null, String(e)); peer.close(); } };
      peer.onclose = () => { if (!closed) { update(null, "Reconnecting"); timer = setTimeout(() => void connect(), 500); } };
    } catch (e) { if (!closed) { update(null, String(e)); timer = setTimeout(() => void connect(), 1000); } }
  }
  await connect();
  return () => { closed = true; clearTimeout(timer); if (socket) { socket.onclose = null; socket.onmessage = null; socket.onopen = null; socket.close(); } binding?.free(); };
}
