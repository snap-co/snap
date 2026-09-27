export type Session = { identified: boolean; csrf?: string; account?: { id: string; owner: string; name: string; email: string }; model: string; model_ready: boolean; files_available: boolean; search_available: boolean };
export type Thread = { id: string; title: string; effort: string; active_turn: string; updated: number };
export type Turn = { id: string; user: string; text: string; summary: string; status: string; error: string; tools: { call_id: string; name: string; arguments: unknown; status: string; result?: unknown }[]; usage: { input_tokens?: number; output_tokens?: number; reasoning_tokens?: number; context_omitted?: number } };
export type View = { thread: Thread; turns: Turn[] };
type Document = { id: string; revision: string; value: Omit<Thread, "id"> & { turns: Turn[] } };
type Result = { documents: Document[]; pending: number; send: string[]; error: string | null };
type Binding = { connect(id: string): string; receive(frame: string): string; rename(id: string, title: string, effort: string): string; free(): void };
type Module = { default(options: { module_or_path: string }): Promise<void>; ChattyClient: new (actor: string) => Binding };
export type Snapshot = { session: Session | null; documents: Document[]; connected: boolean; pending: number; error: string | null };
let modulePromise: Promise<Module> | undefined;
function bindings() {
  return modulePromise ??= (async () => {
    const path = "/bindings/chatty_wasm.js";
    const module = await import(/* @vite-ignore */ path) as Module;
    await module.default({ module_or_path: "/bindings/chatty_wasm_bg.wasm" });
    return module;
  })();
}
export class Chatty {
  private snapshot: Snapshot = { session: null, documents: [], connected: false, pending: 0, error: null };
  private listeners = new Set<() => void>();
  private binding?: Binding;
  private socket?: WebSocket;
  private timer?: ReturnType<typeof setTimeout>;
  private closed = false;
  private epoch = 0;
  private backoff = 250;
  private readonly id = crypto.randomUUID();
  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private set(next: Partial<Snapshot>) { if (this.closed) return; this.snapshot = Object.freeze({ ...this.snapshot, ...next }); for (const listener of this.listeners) listener(); }
  private clear() {
    this.epoch++;
    clearTimeout(this.timer); this.timer = undefined;
    if (this.socket) { this.socket.onclose = null; this.socket.onmessage = null; this.socket.onopen = null; this.socket.close(); this.socket = undefined; }
    this.binding?.free(); this.binding = undefined;
    this.set({ documents: [], connected: false, pending: 0 });
  }
  async start() { await this.refresh(); return this; }
  private async refresh() {
    let epoch = this.epoch;
    try {
      const response = await fetch("/api/session", { credentials: "same-origin" });
      if (!response.ok) throw new Error(`Session check failed (${response.status})`);
      const session = await response.json() as Session;
      if (this.closed || epoch !== this.epoch) return;
      if (!session.identified) { this.clear(); this.set({ session }); return; }
      if (session.account?.owner !== this.snapshot.session?.account?.owner) {
        this.clear();
        epoch = this.epoch;
        const current = this.epoch;
        const module = await bindings();
        if (this.closed || current !== this.epoch) return;
        this.binding = new module.ChattyClient(session.account!.owner);
      }
      this.set({ session, error: null });
      this.connect();
    } catch (error) { if (!this.closed && epoch === this.epoch) { this.set({ error: String(error) }); this.reconnect(); } }
  }
  private reconnect() {
    if (this.closed || this.timer) return;
    this.set({ connected: false });
    const delay = this.backoff; this.backoff = Math.min(this.backoff * 2, 5000);
    this.timer = setTimeout(() => { this.timer = undefined; void this.refresh(); }, delay);
  }
  private connect() {
    if (this.closed || !this.binding || this.socket) return;
    const socket = new WebSocket(`${location.origin.replace(/^http/, "ws")}/transport`);
    this.socket = socket;
    socket.onopen = () => { if (this.socket === socket && this.binding) socket.send(this.binding.connect(this.id)); };
    socket.onmessage = event => {
      if (this.socket !== socket || !this.binding) return;
      try {
        this.apply(this.binding.receive(String(event.data)));
        this.set({ connected: true }); this.backoff = 250;
      } catch (error) { this.set({ error: String(error) }); socket.close(); }
    };
    socket.onclose = () => { if (this.socket === socket) { this.socket = undefined; this.reconnect(); } };
  }
  private apply(raw: string) {
    const result = JSON.parse(raw) as Result;
    this.set({ documents: result.documents, pending: result.pending, error: result.error });
    for (const frame of result.send) if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(frame);
    if (result.error && /InvalidBearer|StaleConnection/.test(result.error)) this.socket?.close();
  }
  view(id: string): View | null {
    const document = this.snapshot.documents.find(d => d.id === id);
    return document ? { thread: { id, ...document.value }, turns: document.value.turns } : null;
  }
  rename(id: string, title: string, effort: string) {
    if (!this.binding || !this.snapshot.connected) throw new Error("Wait for the connection before editing");
    this.apply(this.binding.rename(id, title, effort));
  }
  async command<T>(path: string, body: unknown): Promise<T> {
    const epoch = this.epoch;
    const response = await fetch(path, { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json", "x-snap-csrf": this.snapshot.session?.csrf ?? "" }, body: JSON.stringify(body) });
    const value = await response.json().catch(() => null);
    if (this.closed || epoch !== this.epoch) throw new Error("The session changed during this request");
    if (!response.ok) {
      if (response.status === 401) { this.clear(); this.set({ session: { ...this.snapshot.session!, identified: false } }); }
      throw new Error(value?.error_description ?? `Request failed (${response.status})`);
    }
    return value as T;
  }
  async logout() {
    const result = await this.command<{ redirect: string }>("/auth/logout", {});
    this.clear(); this.set({ session: { ...this.snapshot.session!, identified: false } });
    location.assign(result.redirect);
  }
  close() { this.clear(); this.closed = true; this.listeners.clear(); }
}
