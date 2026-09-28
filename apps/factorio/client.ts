import { Invocations } from "../../platforms/document/client";
export type Ticket = { id: string; title: string; description: string; modules: string[]; status: "draft" | "ready" | "done" | "cancelled"; notes: string; parent: string | null; blockers: string[] };
export type Candidate = { commit: string; target: string; evidence: string; findings: { text: string; disposition: string }[]; approval: { human: string; at: number; commit: string } | null };
export type Session = { id: string; owner: string; prompt: string; tickets: string[]; modules: string[]; phase: string; base: string; branch: string; worktree: string; data: string; port: number; conversation: string; candidate: Candidate | null; integration: string | null; error: string };
export type Intake = { id: string; owner: string; description: string; conversation: string; route: "explore" | "grill" | "triage" | "wayfinder" | "implement"; rationale: string; tickets: string[]; revision: number };
export type Workspace = { config: { repository: string; mainline: string; modules: Record<string, string> }; tickets: Record<string, Ticket>; sessions: Record<string, Session>; intakes?: Record<string, Intake> };
export type Repository = { id: string; path: string; modules: Record<string,string> };
export type Identity = { identified: boolean; csrf?: string; owner?: string; human?: boolean };
type Binding = { connect(id: string): string; invoke(operation: string,input: string): string; receive(text: string): string; free(): void };
type Snapshot = { id: string; kind: string; value: any };
type Update = (workspace: Workspace | null, error?: string) => void;
export function randomID() { return Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join(""); }

/** One logical connection for commands and replication. Application calls use
 * shared ACK/retry/reconnect channels; the Rust SDK owns Document reconciliation. */
export class Factorio {
  identity: Identity = { identified:false };
  workspaceID = "";
  private socket?: WebSocket;
  private binding?: Binding;
  private sequence = 0;
  private lifetime = randomID();
  private ready = false;
  private opening?: Promise<void>;
  private closed = false;
  private timer?: ReturnType<typeof setTimeout>;
  private listeners = new Set<Update>();
  private documents: Record<string,Snapshot> = {};
  private credential?: string;
  private calls = new Invocations((operation,input) => this.binding?.invoke(operation,JSON.stringify(input)) ?? JSON.stringify({Invoke:{id:++this.sequence,operation,input}}), frame => {
    if (!this.ready || this.socket?.readyState !== WebSocket.OPEN) throw new Error("Disconnected");
    this.socket.send(frame);
  });
  constructor(readonly origin: string, private readonly token?: string, private readonly transport: typeof fetch = globalThis.fetch.bind(globalThis)) {}
  private async request<T>(path: string, body?: unknown): Promise<T> {
    const headers: Record<string,string> = {};
    if (this.token) headers.authorization = `Bearer ${this.token}`;
    if (body !== undefined) { headers["content-type"]="application/json"; if (!this.token) headers["x-snap-csrf"]=this.identity.csrf??""; }
    const response = await this.transport(`${this.origin}${path}`,{method:body===undefined?"GET":"POST",headers,body:body===undefined?undefined:JSON.stringify(body),credentials:"same-origin"});
    const value = await response.json().catch(()=>null);
    if (!response.ok) throw new Error(value?.error_description??`Request failed (${response.status})`);
    return value as T;
  }
  async identify() { return this.identity = await this.request<Identity>("/api/session"); }
  private async connect(): Promise<void> {
    if (this.closed) throw new Error("Client closed");
    if (this.ready) return;
    if (this.opening) return this.opening;
    this.opening = (async()=>{
      if (!this.identity.identified) await this.identify();
      if (!this.identity.identified) throw new Error("Sign in to continue");
      if (typeof window !== "undefined" && !this.binding) {
        const path="/bindings/factorio_wasm.js";
        const module=await import(/* @vite-ignore */ path);
        await module.default({module_or_path:"/bindings/factorio_wasm_bg.wasm"});
        this.binding=new module.FactorioClient(this.identity.owner);
      }
      await new Promise<void>((resolve,reject)=>{
        const socket=this.socket=new WebSocket(`${this.origin.replace(/^http/,"ws")}/transport`);
        const timeout=setTimeout(()=>{reject(new Error("Connection timed out"));socket.close();},10000);
        socket.onopen=()=>{
          const command=JSON.parse(this.binding?.connect(this.lifetime)??JSON.stringify({Connect:{bearer:"",client_id:this.lifetime}}));
          command.Connect.bearer=this.token??"";
          socket.send(JSON.stringify(command));
        };
        socket.onmessage=event=>{
          try {
            const frame=String(event.data), response=JSON.parse(frame);
            if (response.Attached) { this.ready=true; clearTimeout(timeout); resolve(); }
            if (response.Failed || response.Detached) throw new Error(JSON.stringify(response.Failed??"Disconnected"));
            if (this.calls.receive(frame)) return;
            if (this.binding) {
              const result=JSON.parse(this.binding.receive(frame));
              this.documents=result.documents;
              for (const send of result.send) socket.send(send);
              this.emit();
            }
          } catch(error) { reject(error); this.emit(String(error)); socket.close(); }
        };
        socket.onerror=()=>reject(new Error("Connection failed"));
        socket.onclose=()=>{
          clearTimeout(timeout); this.ready=false; this.calls.detached();
          reject(new Error("Connection closed"));
          if (!this.closed) { this.emit("Reconnecting"); this.timer=setTimeout(()=>void this.connect().catch(error=>this.emit(String(error))),500); }
        };
      });
    })().finally(()=>{this.opening=undefined;});
    return this.opening;
  }
  async invoke<T>(operation: string, input: unknown): Promise<T> { await this.connect(); return this.calls.invoke<T>(operation,input); }
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
  logout() { return this.request<{redirect:string}>("/auth/logout",{}); }
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
      this.credential??=this.token??(await this.agentToken()).token;
      action={...action,credential:this.credential};
    }
    const value=await this.request(`/api/workspaces/${encodeURIComponent(this.workspaceID)}/intakes/${encodeURIComponent(id)}/opencode`,action);
    if(action.action==="delete") await this.invoke("factorio.intake-delete",{workspace:this.workspaceID,id});
    return value;
  }
  eventsURL(id:string) { return `${this.origin}/api/workspaces/${encodeURIComponent(this.workspaceID)}/intakes/${encodeURIComponent(id)}/events`; }
  watch(update:Update) { this.listeners.add(update);void this.connect().catch(error=>update(null,String(error)));this.emit();return ()=>this.listeners.delete(update); }
  private emit(error?:string) {
    const roots=Object.values(this.documents).filter(d=>d.kind==="factorio.workspace");
    if(!this.workspaceID && roots[0]) this.workspaceID=roots[0].id;
    const root=roots.find(d=>d.id===this.workspaceID)?.value;
    const values=(index:Record<string,string>)=>Object.fromEntries(Object.entries(index).flatMap(([key,id])=>this.documents[id] ? [[key,this.documents[id]!.value.data]] : []));
    const workspace=root ? {config:root.config,tickets:values(root.tickets),sessions:values(root.sessions),intakes:values(root.intakes)} as Workspace : null;
    for(const listener of this.listeners) listener(workspace,error);
  }
  close() { this.closed=true;clearTimeout(this.timer);this.calls.close();this.socket?.close();this.binding?.free();this.binding=undefined; }
}
export async function subscribe(client:Factorio, update:Update) { const stop=client.watch(update);return ()=>{stop();client.close();}; }
