import { BrowserRuntime, type Publication } from "../../../crates/platform/wasm-browser/runtime";
import { wasmModule } from "../../../crates/platform/wasm-browser/wasm";
import { bindIdentityProjection } from "../../../kits/react/identity";
export function randomID() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6]! & 0x0f) | 0x40; bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
export type Session = { identified: boolean; csrf?: string; account?: { id: string; owner: string; name: string; email: string } };
export type Thread = { id: string; title: string; effort: string; active_turn: string; updated: number };
export type Turn = { id: string; user: string; text: string; summary: string; status: string; error: string; tools: { call_id: string; name: string; arguments: unknown; status: string; result?: unknown }[]; usage: { input_tokens?: number; output_tokens?: number; reasoning_tokens?: number; context_omitted?: number } };
export type View = { thread: Thread; turns: Turn[] };
type Document = { id: string; revision: string; value: Omit<Thread, "id"> & { turns: Turn[] } };
type Result = Publication & { documents: Document[]; pending: number };
type Binding = { connect(id: string): string; invoke(operation: string, input: string): string; receive(frame: string): string; free(): void };
type Module = { identity_fetch(origin: string): Promise<string>; default(options: { module_or_path: string }): Promise<void>; ChattyClient: new (actor: string) => Binding };
export type Snapshot = { session: Session | null; documents: Document[]; connected: boolean; pending: number; error: string | null };
const bindings = wasmModule<Module>("chatty");
export class Chatty {
  private snapshot: Snapshot = { session: null, documents: [], connected: false, pending: 0, error: null };
  private listeners = new Set<() => void>();
  readonly runtime = new BrowserRuntime({
    identity: bindIdentityProjection<Session>(async () => (await bindings()).identity_fetch("")),
    key: (session: Session) => session.account!.owner,
    create: async (session: Session) => new (await bindings()).ChattyClient(session.account!.owner),
    decode: (raw: string) => JSON.parse(raw) as Result,
    publish: (result: Result | null) => this.set(result ? { documents: result.documents, pending: result.pending, error: result.error ?? null } : { documents: [], pending: 0, error: null }),
  });
  constructor() {
    this.runtime.subscribe(() => {
      const state = this.runtime.getSnapshot();
      this.set({ session: state.account ?? (state.phase === "anonymous" ? { identified: false } : null), connected: state.connection === "connected", ...(state.error ? { error: state.error } : {}) });
    });
  }
  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private set(next: Partial<Snapshot>) { this.snapshot = Object.freeze({ ...this.snapshot, ...next }); for (const listener of this.listeners) listener(); }
  async start() { await this.runtime.resolve(); return this; }
  view(id: string): View | null {
    const document = this.snapshot.documents.find(d => d.id === id);
    return document ? { thread: { id, ...document.value }, turns: document.value.turns } : null;
  }
  rename(id: string, title: string, _effort: string) { return this.command("chatty.rename", { thread_id: id, title }); }
  command<T>(operation: string, body: unknown): Promise<T> { return this.runtime.invoke<T>(operation, body); }
  async logout() {
    const response = await fetch("/auth/logout", { method: "POST", credentials: "same-origin", headers: { "x-snap-csrf": this.snapshot.session?.csrf ?? "" } });
    if (!response.ok) throw new Error(`Logout failed (${response.status})`);
    const result = await response.json() as { redirect: string };
    await this.runtime.replace(null);
    location.assign(result.redirect);
  }
  close() { this.runtime.close(); this.listeners.clear(); }
}
