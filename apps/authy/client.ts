import { BrowserRuntime, type Publication } from "../../platforms/browser/runtime";
import { wasmModule } from "../../platforms/browser/wasm";
import { Identity } from "../../platforms/identity/client";
import { Transport } from "../../platforms/transport/browser";

export interface Account { identity: string; email: string; profile: string; authenticated_at: number }
export interface ProfileView { name: string; bio: string; revision: string }
export type ConnectionState = "disconnected" | "connecting" | "connected";
export type LogoutScope = "current" | "others" | "all";
export interface SessionSummary { id: string; expires: number; current: boolean }
export interface CredentialSummary { label: string; kind: "password"; removable: boolean }
export interface AuthySnapshot {
  phase: "loading" | "anonymous" | "signed-in" | "error";
  account: Account | null;
  profile: ProfileView | null;
  pending: number;
  connection: ConnectionState;
  saving: boolean;
  error: string | null;
  revision: string;
  sessions: readonly SessionSummary[] | null;
  credentials: readonly CredentialSummary[] | null;
}
interface AuthyBinding {
  connect_command(id: string): string;
  invoke(operation: string, input: string): string;
  receive(frame: string): string;
  enqueue_edit(name: string, bio: string): string;
  free(): void;
}
export interface WasmResult extends Publication {
  snapshot: {
    profile: { revision: string; value: { name: string; bio: string } } | null;
    pending: number;
    revision: string;
    reconciling: boolean;
    needs_recovery: boolean;
  };
}
const loadWasm = wasmModule<{ default(options: { module_or_path: string }): Promise<unknown>; AuthyClient: new (actor: string, profile: string) => AuthyBinding }>("authy");

function safeContinue(value: string | null): string | null {
  if (!value || !value.startsWith("/oauth/")) return null;
  if (value.includes("\\") || value.includes("//") || /[\s<>"]/.test(value)) return null;
  if (!/^\/oauth\/[A-Za-z0-9._~:/?#[\]@!$&'()*+,;=%-]*$/.test(value)) return null;
  return value;
}

export class AuthyClient {
  private listeners = new Set<() => void>();
  private identity = new Identity<Account>(new Transport());
  private reauth = new URLSearchParams(location.search).get("reauth") === "1";
  private busy = false;
  private closed = false;
  private snapshot: AuthySnapshot = { phase: "loading", account: null, profile: null, pending: 0, connection: "disconnected", saving: false, error: null, revision: "0", sessions: null, credentials: null };
  readonly runtime = new BrowserRuntime({
    identity: { fetch: () => this.reauth ? Promise.resolve(null) : this.identity.fetch() },
    key: (account: Account) => account.identity,
    create: async (account: Account) => {
      const module = await loadWasm();
      const binding = new module.AuthyClient(account.identity, account.profile);
      return {
        connect: (id: string) => binding.connect_command(id),
        invoke: (operation: string, input: string) => binding.invoke(operation, input),
        receive: (frame: string) => binding.receive(frame),
        enqueue_edit: (name: string, bio: string) => binding.enqueue_edit(name, bio),
        free: () => binding.free(),
      };
    },
    decode: (raw: string): WasmResult => {
      const result = JSON.parse(raw) as WasmResult;
      return { ...result, ready: !result.snapshot.reconciling && !result.snapshot.needs_recovery };
    },
    publish: (result: WasmResult | null) => {
      const profile = result?.snapshot.profile;
      this.set(result ? { profile: profile ? { ...profile.value, revision: profile.revision } : null, pending: result.snapshot.pending, saving: result.snapshot.pending > 0, revision: result.snapshot.revision, error: result.error ?? null } : { profile: null, pending: 0, saving: false, sessions: null, credentials: null });
    },
  });
  constructor() {
    this.runtime.subscribe(() => {
      const state = this.runtime.getSnapshot();
      this.set({ account: state.account, phase: state.phase === "ready" ? "signed-in" : state.phase, connection: state.connection, ...(state.error ? { error: state.error } : {}) });
    });
  }
  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private set(next: Partial<AuthySnapshot>) { this.snapshot = Object.freeze({ ...this.snapshot, ...next }); for (const listener of this.listeners) listener(); }
  private async acquire(enroll: boolean, email: string, password: string) {
    if (this.busy) throw new Error("Another sign-in action is still pending.");
    this.busy = true;
    try {
      const value = await (enroll ? this.identity.enroll({ email, password }) : this.identity.acquire({ email, password }));
      if (this.closed) return;
      this.reauth = false;
      await this.runtime.replace(value.account);
      const next = safeContinue(new URLSearchParams(location.search).get("continue"));
      if (next) location.assign(next);
    } finally { this.busy = false; }
  }
  signup(email: string, password: string) { return this.acquire(true, email, password); }
  login(email: string, password: string) { return this.acquire(false, email, password); }
  async logout(scope: LogoutScope = "current") {
    if (this.busy) throw new Error("Another sign-in action is still pending.");
    this.busy = true;
    try {
      await this.runtime.invoke("authy.logout", { scope });
      if (scope === "others") { await this.runtime.refresh(); await this.refreshSessions(); }
      else await this.runtime.replace(null);
    } finally { this.busy = false; }
  }
  async refreshSessions() {
    const epoch = this.runtime.getSnapshot().epoch;
    const [sessions, credentials] = await Promise.all([
      this.runtime.invoke<{ sessions: SessionSummary[] }>("authy.sessions", null),
      this.runtime.invoke<{ credentials: CredentialSummary[] }>("authy.credentials", null),
    ]);
    if (epoch === this.runtime.getSnapshot().epoch) this.set({ sessions: sessions.sessions, credentials: credentials.credentials });
  }
  saveProfile(name: string, bio: string) {
    try { this.runtime.mutate(binding => binding.enqueue_edit(name, bio)); }
    catch (error) { this.set({ error: String(error) }); }
  }
  retryConnection() { void this.runtime.refresh(); }
  close() { this.closed = true; this.runtime.close(); this.listeners.clear(); }
}
export async function startAuthy() { const client = new AuthyClient(); await client.runtime.resolve(); return client; }
