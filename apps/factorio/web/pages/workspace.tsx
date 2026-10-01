import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "@tanstack/react-router";
import { Factorio, randomID, type Workspace, type Ticket, type Session, type Repository } from "../client";
import { IntakeDesk } from "./intake";
import { Icon, ListDrawer, ShellNavigation, type Section } from "../shell";

type Run = (action: () => Promise<unknown>) => Promise<void>;

function SessionCard({ client, session: s, busy, run }: { client: Factorio; session: Session; busy: boolean; run: Run }) {
  return <article className="work-detail" id={`session-${s.id}`}>
    <div className="ticket-meta"><h3>{s.id}</h3><span className="badge">{s.phase}</span></div>
    <p className="prose">{s.prompt}</p><p>Claims: {s.modules.join(", ")} · Tickets: {s.tickets.map(id => <Link key={id} to="/tickets/$ticketId" params={{ ticketId: id }}>{id} </Link>)}</p>
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

function TicketEditor({ client, initial, workspace, busy, run, close }: { client: Factorio; initial: Ticket; workspace: Workspace; busy: boolean; run: Run; close: () => void }) {
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

function newestFirst<T extends { id: string; created_at?: number | null }>(items: T[]) {
  return items.sort((a, b) => (b.created_at ?? -Infinity) - (a.created_at ?? -Infinity) || a.id.localeCompare(b.id));
}

export function WorkspacePage({ client, initialRepositories }: { client: Factorio; initialRepositories: Repository[] }) {
  const navigate = useNavigate();
  // Derive section and selection from the same location snapshot. Reading the
  // parent match's parameters separately can combine the old ID with a new path
  // while child route loaders are settling.
  const pathname = useLocation({ select: location => location.pathname });
  const section: Section = pathname.startsWith("/tickets") ? "tickets" : pathname.startsWith("/sessions") ? "sessions" : "intakes";
  const segment = pathname.split("/")[2];
  let selected = "";
  try { selected = segment ? decodeURIComponent(segment) : ""; } catch { selected = segment ?? ""; }
  const params = { intakeId: section === "intakes" ? selected : "", ticketId: section === "tickets" ? selected : "", sessionId: section === "sessions" ? selected : "" };
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [repository, setRepository] = useState(initialRepositories[0]?.id ?? "");
  const [error, setError] = useState("");
  const [connectionError, setConnectionError] = useState("");
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [drawer, setDrawer] = useState(false);
  const [keyboard, setKeyboard] = useState(false);
  const [filters, setFilters] = useState({ tickets: "open", sessions: "open" });
  const [remembered, setRemembered] = useState({ intakes: "", tickets: "", sessions: "" });
  const detail = useRef<HTMLDivElement>(null);
  const closeDrawer = useCallback(() => setDrawer(false), []);
  useEffect(() => client.watch((w, e) => { setWorkspace(w); setConnectionError(e ?? ""); }), [client]);
  useLayoutEffect(() => {
    const viewport = window.visualViewport;
    const resize = () => {
      document.documentElement.style.setProperty("--shell-height", `${viewport?.height ?? innerHeight}px`);
      document.documentElement.style.setProperty("--shell-top", `${viewport?.offsetTop ?? 0}px`);
      const focused = document.activeElement;
      const typing = (focused instanceof HTMLTextAreaElement || focused instanceof HTMLInputElement) && !focused.readOnly;
      setKeyboard(typing && (viewport?.height ?? innerHeight) < innerHeight - 120);
    };
    resize(); viewport?.addEventListener("resize", resize); viewport?.addEventListener("scroll", resize);
    window.addEventListener("resize", resize);
    document.addEventListener("focusin", resize); document.addEventListener("focusout", resize);
    return () => { viewport?.removeEventListener("resize", resize); viewport?.removeEventListener("scroll", resize); window.removeEventListener("resize", resize); document.removeEventListener("focusin", resize); document.removeEventListener("focusout", resize); document.documentElement.style.removeProperty("--shell-height"); document.documentElement.style.removeProperty("--shell-top"); };
  }, []);
  useLayoutEffect(() => {
    setEditing(false); setDrawer(false); detail.current?.scrollTo({ top: 0 });
    const id = selected;
    if (id) setRemembered(previous => previous[section] === id ? previous : { ...previous, [section]: id });
  }, [pathname, section, params.ticketId, params.sessionId, params.intakeId]);
  const tickets = newestFirst(Object.values(workspace?.tickets ?? {}));
  const sessions = newestFirst(Object.values(workspace?.sessions ?? {}));
  const openTickets = tickets.filter(t => !["done", "cancelled"].includes(t.status));
  const openSessions = sessions.filter(s => !["complete", "abandoned"].includes(s.phase));
  const visibleTickets = filters.tickets === "all" ? tickets : openTickets;
  const visibleSessions = filters.sessions === "all" ? sessions : openSessions;
  const selectedTicket = workspace?.tickets[params.ticketId ?? ""];
  const selectedSession = workspace?.sessions[params.sessionId ?? ""];
  useEffect(() => {
    if (!workspace) return;
    if (section === "tickets" && !params.ticketId && visibleTickets[0]) void navigate({ to: "/tickets/$ticketId", params: { ticketId: visibleTickets[0].id }, replace: true });
    if (section === "sessions" && !params.sessionId && visibleSessions[0]) void navigate({ to: "/sessions/$sessionId", params: { sessionId: visibleSessions[0].id }, replace: true });
  }, [workspace, section, params.ticketId, params.sessionId, filters.tickets, filters.sessions]);
  const destinations: Record<Section, string> = {
    intakes: remembered.intakes && workspace?.intakes?.[remembered.intakes] ? `/intakes/${encodeURIComponent(remembered.intakes)}` : "/intakes",
    tickets: remembered.tickets && workspace?.tickets[remembered.tickets] ? `/tickets/${encodeURIComponent(remembered.tickets)}` : "/tickets",
    sessions: remembered.sessions && workspace?.sessions[remembered.sessions] ? `/sessions/${encodeURIComponent(remembered.sessions)}` : "/sessions",
  };
  async function run(action: () => Promise<unknown>) { setBusy(true); setError(""); try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); } }
  async function grill(t: Ticket) {
    const text = `Use grill-me to sharpen ticket ${t.id} before implementation. Keep it a draft until its questions are resolved.\n\n${t.title}\n${t.description}\nModules: ${t.modules.join(", ")}\nBlockers: ${t.blockers.join(", ")}`;
    const intake = Object.values(workspace?.intakes ?? {}).find(item => item.tickets.includes(t.id));
    if (intake) { await client.intakeAction(intake.id, { action: "message", id: `msg_${randomID()}`, text }); void navigate({ to: "/intakes/$intakeId", params: { intakeId: intake.id } }); }
    else { const id = `intake-${randomID()}`; await client.intake(id, text); void navigate({ to: "/intakes/$intakeId", params: { intakeId: id } }); }
  }
  const listSection = section === "tickets" || section === "sessions" ? section : null;
  const listLabel = listSection ? `${filters[listSection] === "all" ? "All" : "Open"} ${listSection}` : "";
  const visibleCount = section === "tickets" ? visibleTickets.length : visibleSessions.length;
  function workList(mobile = false) {
    if (!listSection) return null;
    return <>
      <div className="list-tools"><label htmlFor={mobile ? "drawer-filter" : "sidebar-filter"}>Show {listSection}<select id={mobile ? "drawer-filter" : "sidebar-filter"} value={filters[listSection]} onChange={e => setFilters({ ...filters, [listSection]: e.target.value })}><option value="open">Open</option><option value="all">All</option></select></label><span className="field-hint">Newest first</span></div>
      <nav className="work-list" aria-label={`${listSection === "tickets" ? "Ticket" : "Session"} list`}>
        {section === "tickets" ? visibleTickets.map(t => <Link key={t.id} to="/tickets/$ticketId" params={{ ticketId: t.id }} aria-current={t.id === params.ticketId ? "page" : undefined} onClick={closeDrawer}><strong>{t.title}</strong><span className="list-meta">{t.id} · {t.modules.join(", ")}</span><span className={`list-status ${t.status}`}>{t.status}</span></Link>)
          : visibleSessions.map(s => <Link key={s.id} to="/sessions/$sessionId" params={{ sessionId: s.id }} aria-current={s.id === params.sessionId ? "page" : undefined} onClick={closeDrawer}><strong>{s.id}</strong><span className="list-meta">{s.prompt.split("\n")[0]}</span><span className={`list-status ${s.error ? "blocked" : s.phase}`}>{s.error ? `Blocked · ${s.phase}` : s.phase}</span></Link>)}
        {!visibleCount && <p className="list-empty">No {filters[listSection] === "open" ? "open " : ""}{listSection}.{filters[listSection] === "open" && <button className="quiet" onClick={() => setFilters({ ...filters, [listSection]: "all" })}>Show all {listSection}</button>}</p>}
      </nav>
      <div className="list-footer"><Link className="button" to="/intakes" onClick={closeDrawer}>Describe new work</Link></div>
    </>;
  }
  return <div className={`app-shell${keyboard ? " keyboard-open" : ""}`}>
    <header className="app-header"><div className="app-brand"><span className="brand-name">Factorio</span><div className="workspace-name">{workspace ? <><strong>{workspace.config.repository.split("/").filter(Boolean).at(-1)}</strong><span>{workspace.config.mainline}</span></> : <span className="muted">Track work from idea to review.</span>}</div></div>
      {workspace && <ShellNavigation section={section} destinations={destinations}/>}
      <details className="account-menu"><summary aria-label="Account"><Icon name="account"/><span>Account</span></summary><div className="account-popover"><button disabled={busy} onClick={() => void run(async () => location.assign((await client.logout()).redirect))}>Sign out</button></div></details>
    </header>
    {(error || connectionError) && <div className="shell-notice"><p role="alert">{error || connectionError}</p></div>}
    {!workspace ? <main className="setup-content">{initialRepositories.length ? <section className="onboarding"><h1>Create your first workspace</h1><p>Choose an existing repository on this server. Then describe your first piece of work.</p><form onSubmit={event => { event.preventDefault(); void run(async () => { setWorkspace(await client.onboard(repository)); if (section !== "intakes" && location.pathname === pathname) void navigate({ to: "/intakes" }); }); }}><label>Repository<select aria-label="Repository" value={repository} onChange={event => setRepository(event.target.value)}>{initialRepositories.map(repo => <option key={repo.id} value={repo.id}>{repo.path}</option>)}</select></label><p className="muted">You will own this workspace and its tickets, intakes and sessions.</p><button className="primary" disabled={busy || !repository}>Create workspace</button></form></section> : <div className="workspace-loading" role="status"><div className="loading-line"/><div className="loading-line"/><span>Loading workspace…</span></div>}</main>
      : <>
        <div className={`workspace-layout${listSection ? " has-list" : ""}`}>
          {listSection && <aside className="work-sidebar"><div className="list-heading"><h2>{section === "tickets" ? "Tickets" : "Sessions"}</h2><span className="count">{visibleCount} {filters[listSection]}</span></div>{workList()}</aside>}
          <main className={`workspace-content${params.intakeId ? " conversation-content" : ""}`} ref={detail}>
            {section === "intakes" && <IntakeDesk key={params.intakeId ?? "index"} client={client} workspace={workspace} selected={params.intakeId}/>}
            {section === "tickets" && (selectedTicket ? <article id={`ticket-${selectedTicket.id}`} className="work-detail" key={selectedTicket.id}>
              <div className="ticket-meta"><code>{selectedTicket.id}</code><span className={`badge ${selectedTicket.status}`}>{selectedTicket.status}</span></div><h1>{selectedTicket.title}</h1>
              {editing ? <TicketEditor client={client} initial={selectedTicket} workspace={workspace} busy={busy} run={run} close={() => setEditing(false)}/> : <>
                <p className="prose">{selectedTicket.description}</p>
                <dl className="detail-properties"><div><dt>Modules</dt><dd>{selectedTicket.modules.join(", ")}</dd></div><div><dt>Blockers</dt><dd>{selectedTicket.blockers.length ? selectedTicket.blockers.map(id => <Link key={id} to="/tickets/$ticketId" params={{ ticketId: id }}>{id} ({workspace.tickets[id]?.status ?? "missing"}) </Link>) : "None. No dependencies."}</dd></div></dl>
                {selectedTicket.parent && <p><Link to="/tickets/$ticketId" params={{ ticketId: selectedTicket.parent }}>Parent {selectedTicket.parent}</Link></p>}
                {selectedTicket.notes && <><h2>Implementation notes</h2><p className="prose">{selectedTicket.notes}</p></>}
                <div className="actions detail-actions">
                  {selectedTicket.status === "ready" && <button className="primary" disabled={busy || selectedTicket.modules.length !== 1 || selectedTicket.blockers.some(id => workspace.tickets[id]?.status !== "done") || Object.values(workspace.tickets).some(child => child.parent === selectedTicket.id) || Object.values(workspace.sessions).some(s => !["complete", "abandoned"].includes(s.phase) && s.tickets.includes(selectedTicket.id))} onClick={() => void run(async () => { const id = `work-${randomID()}`; await client.command({ command: "start", id, tickets: [selectedTicket.id], modules: [], prompt: `Implement ticket ${selectedTicket.id}: ${selectedTicket.title}\n${selectedTicket.description}` }); void navigate({ to: "/sessions/$sessionId", params: { sessionId: id } }); })}>Start implementation</button>}
                  {selectedTicket.status === "draft" && <button className="primary" disabled={busy} onClick={() => void run(() => grill(selectedTicket))}>Grill ticket</button>}
                  <button onClick={() => setEditing(true)}>Edit ticket</button>
                </div>
              </>}
              <details className="ticket-more"><summary>More ticket actions</summary><button className="danger quiet" disabled={busy} onClick={() => { if (window.confirm(`Delete ticket ${selectedTicket.id}?`)) void run(async () => { await client.command({ command: "delete_ticket", id: selectedTicket.id }); setEditing(false); setRemembered(previous => ({ ...previous, tickets: "" })); void navigate({ to: "/tickets" }); }); }}>Delete</button></details>
            </article> : <div className="detail-empty"><h1>{params.ticketId ? "Ticket unavailable" : tickets.length ? "No open tickets" : "No tickets yet"}</h1><p>{params.ticketId ? "This ticket was removed or is not available in this workspace." : tickets.length ? "Use the list filter to view completed or cancelled tickets." : "Describe new work to turn an idea into ticket details."}</p><Link className="button" to={params.ticketId ? "/tickets" : "/intakes"}>{params.ticketId ? "Return to tickets" : "Describe new work"}</Link></div>)}
            {section === "sessions" && (selectedSession ? <SessionCard key={selectedSession.id} client={client} session={selectedSession} busy={busy} run={run}/> : <div className="detail-empty"><h1>{params.sessionId ? "Session unavailable" : sessions.length ? "No open sessions" : "No sessions yet"}</h1><p>{params.sessionId ? "This session was removed or is not available in this workspace." : sessions.length ? "Use the list filter to view complete or abandoned sessions." : "Start implementation on a ready ticket. Review and approve its changes here."}</p><Link className="button" to={params.sessionId ? "/sessions" : "/tickets"}>{params.sessionId ? "Return to sessions" : "View tickets"}</Link></div>)}
          </main>
        </div>
        {listSection && <><button className="mobile-list-trigger" aria-haspopup="dialog" aria-expanded={drawer} aria-controls="work-list-drawer" onClick={() => setDrawer(true)}><Icon name={listSection}/><strong>{listLabel}</strong><span className="count">{visibleCount}</span><Icon name="up"/></button><ListDrawer open={drawer} close={closeDrawer} label={listLabel}>{workList(true)}</ListDrawer></>}
        <ShellNavigation section={section} destinations={destinations} mobile/>
      </>}
  </div>;
}
