import { BrowserRuntime, type Publication } from "../../crates/platform/wasm-browser/runtime";
import { wasmModule } from "../../crates/platform/wasm-browser/wasm";
import { OAuthIdentity } from "../../crates/platform/identity/oauth";
export type Ticket = { id: string; created_at?: number | null; title: string; description: string; modules: string[]; status: "draft" | "ready" | "done" | "cancelled"; notes: string; parent: string | null; blockers: string[] };
export type Candidate = { commit: string; target: string; evidence: string; findings: { text: string; disposition: string }[]; approval: { human: string; at: number; commit: string } | null };
export type Session = { id: string; created_at?: number | null; owner: string; prompt: string; tickets: string[]; modules: string[]; phase: string; base: string; branch: string; worktree: string; data: string; port: number; conversation: string; candidate: Candidate | null; integration: string | null; error: string };
export type Intake = { id: string; owner: string; description: string; conversation: string; route: "explore" | "grill" | "triage" | "wayfinder" | "implement"; rationale: string; tickets: string[]; revision: number };
export type Workspace = { config: { repository: string; mainline: string; modules: Record<string, string> }; tickets: Record<string, Ticket>; sessions: Record<string, Session>; intakes?: Record<string, Intake> };
export type Repository = { id: string; path: string; modules: Record<string,string> };
export type Identity = { identified: boolean; csrf?: string; owner?: string; human?: boolean };
type Binding = { connect(id: string): string; invoke(operation: string,input: string): string; receive(text: string): string; free(): void };
type Snapshot = { id: string; kind: string; value: any };
type Result = Publication & { documents: Record<string, Snapshot> };
const bindings = wasmModule<{ default(options: { module_or_path: string }): Promise<void>; FactorioClient: new (actor: string) => Binding }>("factorio");
type Update = (workspace: Workspace | null, error?: string) => void;
export function randomID() { return Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join(""); }

/** Browser application adapter. Snap owns the connection lifecycle and the Rust
 * SDK owns Document reconciliation; Factorio supplies commands and workspace views. */
export class Factorio {
  identity: Identity = { identified:false };
  workspaceID = "";
  private listeners = new Set<Update>();
  private documents: Record<string,Snapshot> = {};
  private credential?: string;
  readonly runtime: BrowserRuntime<Identity, Binding, Result>;
  constructor(readonly origin: string, private readonly transport: typeof fetch = globalThis.fetch.bind(globalThis)) {
    this.runtime = new BrowserRuntime({
      identity: new OAuthIdentity<Identity>(origin, transport),
      key: identity => identity.owner!,
      create: async identity => new (await bindings()).FactorioClient(identity.owner!),
      decode: raw => JSON.parse(raw) as Result,
      publish: result => {
        this.documents = result?.documents ?? {};
        if (!result) { this.workspaceID = ""; this.credential = undefined; }
        this.emit(result?.error ?? undefined);
      },
    });
    this.runtime.subscribe(() => {
      const state = this.runtime.getSnapshot();
      this.identity = state.account ?? { identified: false };
      this.emit(state.error ?? (state.phase === "ready" && state.connection !== "connected" ? "Reconnecting" : undefined));
    });
  }
  private async request<T>(path: string, body?: unknown): Promise<T> {
    const headers: Record<string,string> = {};
    if (body !== undefined) { headers["content-type"]="application/json"; headers["x-snap-csrf"]=this.identity.csrf??""; }
    const response = await this.transport(`${this.origin}${path}`,{method:body===undefined?"GET":"POST",headers,body:body===undefined?undefined:JSON.stringify(body),credentials:"same-origin"});
    const value = await response.json().catch(()=>null);
    if (!response.ok) throw new Error(value?.error_description??`Request failed (${response.status})`);
    return value as T;
  }
  async identify() {
    await this.runtime.resolve();
    return this.identity;
  }
  async invoke<T>(operation: string, input: unknown): Promise<T> {
    await this.runtime.resolve();
    if (!this.identity.identified) throw new Error("Sign in to continue");
    return this.runtime.invoke<T>(operation,input);
  }
  repositories() { return this.invoke<Repository[]>("factorio.repositories",{}); }
  async workspaces() { return this.invoke<{id:string;repository:string}[]>("factorio.workspaces",{}); }
  async onboard(repository: string) { const value=await this.invoke<{id:string}>("factorio.onboard",{repository});this.workspaceID=value.id;return this.workspace(); }
  async workspace(): Promise<Workspace> {
    if (!this.workspaceID) this.workspaceID=(await this.workspaces())[0]?.id??"";
    if (!this.workspaceID) throw new Error("Create your first workspace");
    return this.invoke<Workspace>("factorio.workspace",{workspace:this.workspaceID});
  }
  async command(command: Record<string,unknown>) {
    await this.workspace();
    try { await this.invoke("factorio.command",{workspace:this.workspaceID,command}); }
    catch(error) {
      let outcome: {Application?:{code?:string;committed?:boolean}} = {};
      try { outcome=JSON.parse(error instanceof Error?error.message:""); } catch {}
      if(outcome.Application?.code!=="Blocked" || !outcome.Application.committed) throw error;
      const state=await this.workspace();
      throw new Error(state.sessions[String(command.id??"")]?.error || "Controller blocked. Inspect the session and retry.");
    }
    const workspace=await this.workspace();
    const session=workspace.sessions[String(command.id??"")];
    if(session?.error) throw new Error(session.error);
    return workspace;
  }
  approve(id:string,commit:string) { return this.command({command:"approve",id,commit}); }
  agentToken() { return this.invoke<{token:string}>("factorio.agent-token",{}); }
  async logout() { const result = await this.request<{redirect:string}>("/auth/logout",{}); await this.runtime.replace(null); return result; }
  async intake(id:string,description:string) {
    await this.workspace();
    const intake=await this.invoke<Intake>("factorio.intake-create",{workspace:this.workspaceID,id,description});
    await this.intakeAction(id,{action:"resume"});
    return intake;
  }
  async intakeRead(id:string) { await this.workspace();return this.invoke<{intake:Intake;modules:Record<string,string>;tickets:Record<string,Ticket>}>("factorio.intake-read",{workspace:this.workspaceID,id}); }
  async intakeDrafts(id:string,drafts:unknown) { await this.workspace();return this.invoke<Intake>("factorio.intake-drafts",{workspace:this.workspaceID,id,drafts}); }
  async intakeAction(id:string,action:Record<string,unknown>) {
    await this.workspace();
    if(action.action==="ready") return this.invoke("factorio.intake-ready",{workspace:this.workspaceID,id,revision:action.revision});
    if(action.action==="resume" || action.action==="message") {
      this.credential??=(await this.agentToken()).token;
      action={...action,credential:this.credential};
    }
    const value=await this.request(`/api/workspaces/${encodeURIComponent(this.workspaceID)}/intakes/${encodeURIComponent(id)}/opencode`,action);
    if(action.action==="delete") await this.invoke("factorio.intake-delete",{workspace:this.workspaceID,id});
    return value;
  }
  eventsURL(id:string) { return `${this.origin}/api/workspaces/${encodeURIComponent(this.workspaceID)}/intakes/${encodeURIComponent(id)}/events`; }
  watch(update:Update) { this.listeners.add(update);void this.runtime.resolve().catch(error=>update(null,String(error)));this.emit();return ()=>{ this.listeners.delete(update); }; }
  private emit(error?:string) {
    const roots=Object.values(this.documents).filter(d=>d.kind==="factorio.workspace");
    if(!this.workspaceID && roots[0]) this.workspaceID=roots[0].id;
    const root=roots.find(d=>d.id===this.workspaceID)?.value;
    const values=(index:Record<string,string>)=>Object.fromEntries(Object.entries(index).flatMap(([key,id])=>this.documents[id] ? [[key,this.documents[id]!.value.data]] : []));
    const workspace=root ? {config:root.config,tickets:values(root.tickets),sessions:values(root.sessions),intakes:values(root.intakes)} as Workspace : null;
    for(const listener of this.listeners) listener(workspace,error);
  }
  close() { this.runtime.close();this.listeners.clear(); }
}
export async function subscribe(client:Factorio, update:Update) { const stop=client.watch(update);return ()=>{stop();client.close();}; }
