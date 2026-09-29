import React, { useEffect, useRef, useState } from "react";
import { Link, useNavigate } from "@tanstack/react-router";
import { Factorio, randomID, type Workspace } from "../client";

type Answer = Record<string, string | number | boolean | string[]>;
type Field = { key: string; type: string; title?: string; description?: string; required?: boolean; hidden?: boolean; default?: Answer[string]; options?: { value: string; label: string }[]; custom?: boolean; url?: string; when?: { key: string; op: "eq" | "neq"; value: unknown }[] };
type Question = { id: string; title: string; fields: Field[] };
type Conversation = {
  messages: { id: string; role: string; text?: string; parts?: { type: string; text?: string; name?: string; status?: string }[]; error?: string }[];
  forms: Question[];
  permissions: { id: string; action: string; resources: string[] }[];
  outcome?: string;
  model?: { providerID: string; id: string; variant?: string };
};

function QuestionForm({ question, reply, busy }: { question: Question; reply: (answer: Answer) => void; busy: boolean }) {
  const [answer, setAnswer] = useState<Answer>(() => Object.fromEntries(question.fields.filter(f => f.default !== undefined).map(f => [f.key, f.default!])));
  const [custom, setCustom] = useState<Record<string, string>>(() => Object.fromEntries(question.fields.filter(f => f.type === "multiselect" && f.custom).map(f => [f.key, (Array.isArray(f.default) ? f.default : []).filter(v => !f.options?.some(o => o.value === v)).join(", ")])));
  const customValues = (value: string) => value.split(",").map(s => s.trim()).filter(Boolean);
  // OpenCode conditions are evaluated against earlier active answers. Hidden
  // presentation does not override conditional activity or make a field answerable.
  const active = new Set<string>(), activeAnswers: Answer = {};
  for (const field of question.fields) {
    if (!(field.when ?? []).every(w => {
      const value = activeAnswers[w.key];
      if (value === undefined) return false;
      const equal = Array.isArray(value) ? value.some(v => v === w.value) : value === w.value;
      return w.op === "eq" ? equal : !equal;
    })) continue;
    active.add(field.key);
    if (answer[field.key] !== undefined) activeAnswers[field.key] = answer[field.key]!;
  }
  return <form className="question" onSubmit={event => { event.preventDefault(); reply(activeAnswers); }}>
    <h4>{question.title}</h4><fieldset disabled={busy}>
    {question.fields.filter(f => active.has(f.key) && !f.hidden).map(f => <label key={f.key}>{f.title ?? f.key}{f.description && <span className="field-hint">{f.description}</span>}
      {f.type === "boolean" ? <select value={String(answer[f.key] ?? "")} required={f.required} onChange={e => { const next = { ...answer }; if (e.target.value === "") delete next[f.key]; else next[f.key] = e.target.value === "true"; setAnswer(next); }}><option value="">Choose</option><option value="true">Yes</option><option value="false">No</option></select>
      : f.type === "multiselect" ? <><select multiple aria-label={f.title ?? f.key} required={f.required && (!f.custom || customValues(custom[f.key] ?? "").length === 0)} value={(Array.isArray(answer[f.key]) ? answer[f.key] as string[] : []).filter(v => f.options?.some(o => o.value === v))} onChange={e => setAnswer({ ...answer, [f.key]: [...new Set([...Array.from(e.target.selectedOptions, o => o.value), ...(f.custom ? customValues(custom[f.key] ?? "") : [])])] })}>{f.options?.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}</select>{f.custom && <input aria-label={`${f.title ?? f.key} custom choices`} placeholder="Other choices, comma separated" value={custom[f.key] ?? ""} onChange={e => { setCustom({ ...custom, [f.key]: e.target.value }); setAnswer({ ...answer, [f.key]: [...new Set([...(Array.isArray(answer[f.key]) ? answer[f.key] as string[] : []).filter(v => f.options?.some(o => o.value === v)), ...customValues(e.target.value)])] }); }}/>}</>
      : f.type === "string" && f.options && !f.custom ? <select required={f.required} value={String(answer[f.key] ?? "")} onChange={e => { const next = { ...answer }; if (e.target.value === "") delete next[f.key]; else next[f.key] = e.target.value; setAnswer(next); }}><option value="">Choose</option>{f.options.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}</select>
      : f.type === "external" ? (f.url && /^https?:\/\//.test(f.url) ? <a href={f.url} target="_blank" rel="noreferrer">Open in a new tab</a> : <span>Continue this step in OpenCode.</span>)
      : ["string", "number", "integer"].includes(f.type) ? <input type={f.type === "string" ? "text" : "number"} step={f.type === "integer" ? 1 : "any"} required={f.required} value={String(answer[f.key] ?? "")} onChange={e => { const next = { ...answer }; if (e.target.value === "") delete next[f.key]; else next[f.key] = f.type === "string" ? e.target.value : Number(e.target.value); setAnswer(next); }}/>
      : <span>Continue this step in OpenCode.</span>}
    </label>)}<button className="primary">Send answers</button></fieldset>
  </form>;
}

export function IntakeDesk({ client, workspace, selected = "" }: { client: Factorio; workspace: Workspace; selected?: string }) {
  const navigate = useNavigate();
  const [description, setDescription] = useState("");
  const [text, setText] = useState("");
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [error, setError] = useState("");
  const [connection, setConnection] = useState("Connecting to OpenCode…");
  const [busy, setBusy] = useState(false);
  const pending = useRef<{ id: string; text: string } | null>(null);
  const creating = useRef<{ id: string; text: string } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [newMessages, setNewMessages] = useState(false);
  const intakes = Object.values(workspace.intakes ?? {}).filter(i => i.owner === client.identity.owner);
  const intake = intakes.find(i => i.id === selected);
  useEffect(() => {
    if (!selected) return;
    const viewport = window.visualViewport;
    const resize = () => {
      document.documentElement.style.setProperty("--thread-height", `${viewport?.height ?? window.innerHeight}px`);
      document.documentElement.style.setProperty("--thread-top", `${viewport?.offsetTop ?? 0}px`);
    };
    resize(); viewport?.addEventListener("resize", resize); viewport?.addEventListener("scroll", resize);
    return () => { viewport?.removeEventListener("resize", resize); viewport?.removeEventListener("scroll", resize); document.documentElement.style.removeProperty("--thread-height"); document.documentElement.style.removeProperty("--thread-top"); };
  }, [selected]);
  useEffect(() => {
    if (follow.current) scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
    else setNewMessages(true);
  }, [conversation]);
  useEffect(() => {
    setConversation(null);
    if (!intake) return;
    setConnection("Connecting to OpenCode…");
    const events = new EventSource(client.eventsURL(intake.id));
    events.addEventListener("snapshot", event => { setConversation(JSON.parse(event.data)); setConnection("Connected to OpenCode"); });
    events.addEventListener("unavailable", event => setConnection(event.data));
    events.addEventListener("expired", () => { setError("Your session expired. Sign in again to continue."); events.close(); });
    events.onerror = () => setConnection("Reconnecting to OpenCode…");
    return () => events.close();
  }, [client, intake?.id]);
  async function run(action: () => Promise<unknown>) { setBusy(true); setError(""); try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); } }
  async function start() {
    if (!creating.current || creating.current.text !== description) creating.current = { id: `intake-${randomID()}`, text: description };
    await client.intake(creating.current.id, description);
    void navigate({ to: "/intakes/$intakeId", params: { intakeId: creating.current.id } });
    setDescription(""); creating.current = null;
  }
  const action = (value: Record<string, unknown>) => client.intakeAction(intake!.id, value);
  if (selected && !intake) return <section className="intake-desk"><Link to="/" hash="intake">← Workspace</Link><p role="status">Conversation unavailable. Return to the workspace to refresh or start again.</p></section>;
  return <section id="intake" className={intake ? "intake-thread" : "intake-desk"}>
    {!intake && <><div className="section-heading"><h2>What would you like to work on?</h2></div>
    <p className="muted">Describe the change. The agent will explore the code, ask what it needs to know, and draft the tickets.</p>
    {intakes.length > 0 && <nav className="thread-list" aria-label="Conversations">{intakes.map(i => <Link key={i.id} to="/intakes/$intakeId" params={{ intakeId: i.id }}><span>{i.description.slice(0, 120)}</span><span className="badge">{i.route}</span></Link>)}</nav>}
    {!intake && <form onSubmit={e => { e.preventDefault(); void run(start); }}><label>Your idea<textarea aria-label="Your idea" rows={4} required maxLength={16384} placeholder="What should change, and why? Rough ideas are welcome." value={description} onChange={e => setDescription(e.target.value)}/></label><button className="primary" disabled={busy}>{busy ? "Starting…" : "Work through this"}</button></form>}
    {error && <p role="alert">{error}</p>}</>}
    {intake && <>
      <header className="thread-header"><Link className="button quiet" to="/" hash="intake" aria-label="Back to workspace">← Back</Link><div><h1>{intake.description}</h1><span role="status">{connection}</span></div><span className="badge">{intake.route}</span></header>
      <div className="thread-scroll" ref={scroller} onScroll={() => { const s = scroller.current!; follow.current = s.scrollHeight - s.scrollTop - s.clientHeight < 80; if (follow.current) setNewMessages(false); }}>
      <div className="thread-content">
      <details className="thread-details"><summary>Drafts and session details{intake.tickets.length ? ` · ${intake.tickets.length}` : ""}</summary>
      {intake.rationale && <p>{intake.rationale}</p>}
      {intake.tickets.map(id => <p key={id}><Link to="/" hash={`ticket-${id}`}>{workspace.tickets[id]?.title ?? id}</Link> <span className="badge">{workspace.tickets[id]?.status ?? "removed"}</span></p>)}
      <p>Full history in OpenCode:</p><code>opencode --session {intake.conversation}</code><p><button disabled={busy} onClick={() => void run(() => action({ action: "resume" }))}>Reconnect session</button></p>
      <button className="danger quiet" disabled={busy} onClick={() => { if (window.confirm("Delete this conversation? Any saved tickets will remain.")) void run(async () => { await action({action:"delete"}); void navigate({ to: "/", hash: "intake" }); }); }}>Delete conversation</button>
      </details>
      <div className="conversation" aria-label="Intake conversation">
        {!conversation?.messages.length && <article className="chat-user"><strong>You</strong><p className="prose">{intake.description}</p></article>}
        {conversation?.messages.map(message => <article className={`chat-${message.role}`} key={message.id}><strong>{message.role === "user" ? "You" : "OpenCode"}</strong>{message.text && <p className="prose">{message.text}</p>}{message.parts?.map((part, index) => part.type === "text" ? <p className="prose" key={index}>{part.text}</p> : <p className="tool-status" key={index}>{part.name ?? "Tool"} · {part.status ?? "working"}</p>)}{message.error && <p role="alert">{message.error}</p>}</article>)}
      </div>
      {conversation?.forms.map(q => <QuestionForm key={q.id} question={q} busy={busy} reply={answer => void run(() => action({ action: "form", id: q.id, reply: { answer } }))}/>)}
      {conversation?.permissions.map(p => <article key={p.id}><h4>OpenCode needs permission</h4><p>{p.action}</p><pre>{p.resources.join("\n")}</pre><div className="actions"><button disabled={busy} onClick={() => void run(() => action({ action: "permission", id: p.id, reply: "once" }))}>Allow once</button><button disabled={busy} onClick={() => void run(() => action({ action: "permission", id: p.id, reply: "reject" }))}>Decline</button></div></article>)}
      {intake.tickets.length > 0 && <div className="draft-summary"><h3>Drafted tickets</h3>{intake.tickets.map(id => <p key={id}><Link to="/" hash={`ticket-${id}`}>{workspace.tickets[id]?.title ?? id}</Link> <span className="badge">{workspace.tickets[id]?.status ?? "removed"}</span></p>)}{intake.route === "implement" && intake.tickets.some(id => workspace.tickets[id]?.status === "draft" && !Object.values(workspace.tickets).some(t => t.parent === id)) && <button disabled={busy} className="primary" onClick={() => void run(() => action({ action: "ready", revision: intake.revision }))}>Mark implementation tickets ready</button>}</div>}
      </div></div>
      {newMessages && <button className="latest-message" onClick={() => { follow.current = true; setNewMessages(false); scroller.current?.scrollTo({top:scroller.current.scrollHeight}); }}>Latest messages ↓</button>}
      <div className="thread-bottom">{error && <p role="alert">{error}</p>}<form className="thread-composer" onSubmit={e => { e.preventDefault(); void run(async () => { if (!pending.current || pending.current.text !== text) pending.current = { id: `msg_${randomID()}`, text }; await action({ action: "message", ...pending.current }); setText(""); pending.current = null; follow.current = true; }); }}><label className="reply-label">Reply<textarea aria-label="Reply" rows={2} maxLength={16384} required value={text} onChange={e => setText(e.target.value)} placeholder="Reply or add context…"/></label><div className="composer-footer"><span className="composer-model" aria-label="OpenCode model">{conversation?.model ? <>{conversation.model.providerID}/<wbr/>{conversation.model.id}{conversation.model.variant && ` · ${conversation.model.variant}`}</> : "Model unavailable"}</span><div className="actions"><button type="button" disabled={busy} onClick={() => void run(() => action({ action: "interrupt" }))}>Stop</button><button className="primary" disabled={busy || !text.trim()}>Send reply</button></div></div></form></div>
    </>}
  </section>;
}
