import React, { useEffect, useLayoutEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Factorio, randomID, subscribe, type Workspace, type Ticket, type Session, type Repository } from "../client";
import { IntakeDesk } from "./intake";
import "./style.css";

const client = new Factorio(location.origin);
type Run = (action: () => Promise<unknown>) => Promise<void>;

function SessionCard({ session: s, busy, run }: { session: Session; busy: boolean; run: Run }) {
  return <article>
    <div className="ticket-meta"><h3>{s.id}</h3><span className="badge">{s.phase}</span></div>
    <p className="prose">{s.prompt}</p><p>Claims: {s.modules.join(", ")} · Tickets: {s.tickets.join(", ")}</p>
    <details><summary>Worktree and conversation</summary><p>Worktree: <code>{s.worktree}</code><br/>Data: <code>{s.data}</code> · Port: {s.port}</p><p>OpenCode: <code>opencode --session {s.conversation}</code></p></details>
    {s.error && <p role="alert">{s.error}</p>}
    {s.candidate && <div className="candidate"><h4>Candidate for review</h4><p>Candidate: <code>{s.candidate.commit}</code><br/>Mainline at publication: <code>{s.candidate.target}</code></p><pre>{s.candidate.evidence}</pre>
      {s.candidate.findings.map((f, i) => <p key={i}>{f.text}: {f.disposition || "Unresolved"}</p>)}
      {s.candidate.approval ? <p>Approved by {s.candidate.approval.human} for {s.candidate.approval.commit}</p> : s.phase === "published" && <button className="primary" disabled={busy} onClick={() => { if (window.confirm(`I have reviewed and approve candidate ${s.candidate!.commit} for local integration.`)) void run(() => client.approve(s.id, s.candidate!.commit)); }}>Approve candidate as human</button>}
    </div>}
    {s.integration && <p>Integrated commit: <code>{s.integration}</code></p>}
    <div className="actions">{["accept", "recover", "cleanup", "abandon"].map(command => <button className={command === "abandon" ? "danger quiet" : ""} key={command} disabled={busy} onClick={() => { if (command !== "abandon" || window.confirm("Abandon this attempt and release its claims? Unmerged work will be preserved.")) void run(() => client.command({ command, id: s.id })); }}>{command}</button>)}</div>
  </article>;
}

function TicketEditor({ initial, workspace, busy, run, close }: { initial: Ticket; workspace: Workspace; busy: boolean; run: Run; close: () => void }) {
  const [ticket, setTicket] = useState(initial);
  const [modules, setModules] = useState(initial.modules.join(", "));
  const [blockers, setBlockers] = useState(initial.blockers.join(", "));
  const split = (value: string) => value.split(",").map(s => s.trim()).filter(Boolean);
  return <form id="ticket-editor" onSubmit={e => { e.preventDefault(); void run(async () => { await client.command({ command: "ticket", ticket: { ...ticket, modules: split(modules), blockers: split(blockers) } }); close(); }); }}>
    <h3>Edit ticket</h3><fieldset disabled={busy}>
      <p><code>{ticket.id}</code></p>
      <label>Title<input aria-label="title" required value={ticket.title} onChange={e => setTicket({ ...ticket, title: e.target.value })}/></label>
      <label>Description<textarea aria-label="description" rows={4} value={ticket.description} onChange={e => setTicket({ ...ticket, description: e.target.value })}/></label>
      <label>Status<select value={ticket.status} onChange={e => setTicket({ ...ticket, status: e.target.value as Ticket["status"] })}><option value="draft">Draft</option><option value="ready">Ready</option><option value="cancelled">Cancelled</option>{ticket.status === "done" && <option value="done">Done</option>}</select></label>
      <label>Modules<input aria-label="Modules" required value={modules} onChange={e => setModules(e.target.value)}/></label>
      <details><summary>Available modules</summary><p className="module-list">{Object.keys(workspace.config.modules).join(", ")}</p><p className="muted">Use * for repository-wide work.</p></details>
      <details><summary>Dependencies and notes</summary><label>Blockers<input aria-label="Blockers" value={blockers} onChange={e => setBlockers(e.target.value)}/></label><label>Parent<input value={ticket.parent ?? ""} onChange={e => setTicket({ ...ticket, parent: e.target.value || null })}/></label><label>Notes<textarea aria-label="notes" rows={3} value={ticket.notes} onChange={e => setTicket({ ...ticket, notes: e.target.value })}/></label></details>
      <div className="actions"><button className="primary">Save ticket</button><button type="button" onClick={close}>Cancel</button></div>
    </fieldset>
  </form>;
}

function App() {
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [repositories, setRepositories] = useState<Repository[] | null>(null);
  const [repository, setRepository] = useState("");
  const [identified, setIdentified] = useState(false);
  const [error, setError] = useState("");
  const [ticket, setTicket] = useState<Ticket | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [hash, setHash] = useState(location.hash);
  useEffect(() => { const changed = () => setHash(location.hash); window.addEventListener("hashchange", changed); return () => window.removeEventListener("hashchange", changed); }, []);
  // Thread routes unmount workspace anchors. Scroll only after the destination
  // DOM exists, including direct links restored after the initial snapshot.
  useLayoutEffect(() => {
    if (workspace && !hash.startsWith("#intake-")) document.getElementById(hash.slice(1))?.scrollIntoView({block:"start"});
  }, [hash, Boolean(workspace)]);
  useEffect(() => {
    let close: (() => void) | undefined, disposed = false;
    void client.identify().then(async identity => {
      if (disposed) return;
      setIdentified(identity.identified);
      if (identity.identified) {
        const roots = await client.workspaces();
        if (roots.length === 0) {
          const choices = await client.repositories();
          if (!disposed) { setRepositories(choices); setRepository(choices[0]?.id ?? ""); }
        } else client.workspaceID = roots[0]!.id;
        const stop = await subscribe(client, (w, e) => { if (!disposed) { setWorkspace(w); setError(e ?? ""); } });
        if (disposed) stop(); else close = stop;
      }
    }).catch(e => setError(String(e)));
    return () => { disposed = true; close?.(); };
  }, []);
  async function run(action: () => Promise<unknown>) { setBusy(true); setError(""); try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); } }
  async function grill(t: Ticket) {
    const text=`Use grill-me to sharpen ticket ${t.id} before implementation. Keep it a draft until its questions are resolved.\n\n${t.title}\n${t.description}\nModules: ${t.modules.join(", ")}\nBlockers: ${t.blockers.join(", ")}`;
    const intake=Object.values(workspace?.intakes??{}).find(item=>item.tickets.includes(t.id));
    if(intake) { await client.intakeAction(intake.id,{action:"message",id:`msg_${randomID()}`,text});location.hash=intake.id; }
    else { const id=`intake-${randomID()}`;await client.intake(id,text);location.hash=id; }
  }
  if (workspace && hash.startsWith("#intake-")) return <main className="thread-shell"><IntakeDesk key={hash} client={client} workspace={workspace} selected={hash.slice(1)}/></main>;
  return <main>
    <header><div><h1>Factorio</h1><p className="subtitle">Track work from idea to review.</p></div>
      {identified ? <div className="account-actions"><button disabled={busy} onClick={() => void run(async () => setToken((await client.agentToken()).token))}>Create agent token</button><button disabled={busy} onClick={() => void run(async () => location.assign((await client.logout()).redirect))}>Sign out</button></div> : <a className="button primary" href="/auth/login">Continue with Authy</a>}
    </header>
    {error && <p role="alert">{error}</p>}
    {token && <label>Agent token. Copy once, then dismiss.<input aria-label="Agent token" type="password" readOnly value={token}/><button onClick={() => setToken("")}>Dismiss token</button></label>}
    {identified && !workspace && repositories && <section className="onboarding"><h2>Create your first workspace</h2><p>Choose an existing repository on this server. Then describe your first piece of work.</p><form onSubmit={event => { event.preventDefault(); void run(async () => { setWorkspace(await client.onboard(repository)); setRepositories(null); location.hash="intake"; }); }}><label>Repository<select aria-label="Repository" value={repository} onChange={event => setRepository(event.target.value)}>{repositories.map(repo => <option key={repo.id} value={repo.id}>{repo.path}</option>)}</select></label><p className="muted">You will own this workspace and its tickets, intakes and sessions.</p><button className="primary" disabled={busy || !repository}>Create workspace</button></form></section>}
    {identified && !workspace && !repositories && !error && <p role="status">Loading workspace…</p>}
    {workspace && <>
      <div className="workspace-bar"><span>{workspace.config.repository.split("/").filter(Boolean).at(-1)}</span><code>{workspace.config.mainline}</code><nav aria-label="Workspace"><a href="#tickets">Tickets <span className="count">{Object.keys(workspace.tickets).length}</span></a><a href="#sessions">Sessions <span className="count">{Object.keys(workspace.sessions).length}</span></a></nav></div>
      <IntakeDesk client={client} workspace={workspace}/>
      <section id="tickets"><div className="section-heading"><h2>Tickets</h2><a className="button" href="#intake">Describe new work</a></div>
        <div className={ticket ? "columns" : "ticket-list"}><div>
          {Object.keys(workspace.tickets).length === 0 && <div className="empty-state"><h3>No tickets yet</h3><p>Start with a description above. Your conversation will produce the ticket details.</p></div>}
          {Object.values(workspace.tickets).map(t => <article id={`ticket-${t.id}`} key={t.id}>
            <div className="ticket-meta"><code>{t.id}</code><span className={`badge ${t.status}`}>{t.status}</span></div><h3>{t.title}</h3><p className="muted">{t.modules.join(", ")}</p><p className="prose">{t.description}</p>
            {t.blockers.length > 0 && <p>Blockers: {t.blockers.map(id => <a key={id} href={`#ticket-${id}`}>{id} ({workspace.tickets[id]?.status}) </a>)}</p>}
            {t.parent && <a href={`#ticket-${t.parent}`}>Parent {t.parent}</a>}{t.notes && <p className="prose muted">{t.notes}</p>}
            <div className="actions"><a className="button" href="#ticket-editor" onClick={() => setTicket(t)}>Edit</a>
              {t.status === "draft" && <button disabled={busy} onClick={() => void run(() => grill(t))}>Grill ticket</button>}
              {t.status === "ready" && <button className="primary" disabled={busy || t.blockers.some(id => workspace.tickets[id]?.status !== "done") || Object.values(workspace.tickets).some(child => child.parent === t.id) || Object.values(workspace.sessions).some(s => !["complete","abandoned"].includes(s.phase) && s.tickets.includes(t.id))} onClick={() => void run(async () => { await client.command({command:"start", id:`work-${randomID()}`, tickets:[t.id],modules:[],prompt:`Implement ticket ${t.id}: ${t.title}\n${t.description}`}); location.hash="sessions"; })}>Start implementation</button>}
              <button className="danger quiet" disabled={busy} onClick={() => void run(async () => { await client.command({ command: "delete_ticket", id: t.id }); if (ticket?.id === t.id) setTicket(null); })}>Delete</button></div>
          </article>)}
        </div>{ticket && <TicketEditor key={ticket.id} initial={ticket} workspace={workspace} busy={busy} run={run} close={() => setTicket(null)}/>}</div>
      </section>
      <section id="sessions"><h2>Sessions</h2><p className="muted">Start and publish through <code>bin/factory</code>. Review and approve changes here.</p>
        {Object.keys(workspace.sessions).length === 0 && <div className="empty-state"><h3>No sessions yet</h3><p>Sessions appear here when work starts on a ticket.</p></div>}
        {Object.values(workspace.sessions).map(s => <SessionCard key={s.id} session={s} busy={busy} run={run}/>)}
      </section>
    </>}
  </main>;
}
createRoot(document.getElementById("root")!).render(<App/>);
