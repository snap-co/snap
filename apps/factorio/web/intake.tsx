import React, { useEffect, useRef, useState } from "react";
import { Factorio, randomID, type Intake, type Workspace } from "../client";

type Answer = Record<string, string | number | boolean | string[]>;
type Field = { key: string; type: string; title?: string; description?: string; required?: boolean; hidden?: boolean; default?: Answer[string]; options?: { value: string; label: string }[]; custom?: boolean; url?: string; when?: { key: string; op: "eq" | "neq"; value: unknown }[] };
type Question = { id: string; title: string; fields: Field[] };
type Conversation = {
  messages: { id: string; role: string; text?: string; parts?: { type: string; text?: string; name?: string; status?: string }[]; error?: string }[];
  forms: Question[];
  permissions: { id: string; action: string; resources: string[] }[];
  outcome?: string;
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

export function IntakeDesk({ client, workspace }: { client: Factorio; workspace: Workspace }) {
  const [selected, select] = useState(() => location.hash.startsWith("#intake-") ? location.hash.slice(1) : "");
  const [description, setDescription] = useState("");
  const [text, setText] = useState("");
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [error, setError] = useState("");
  const [connection, setConnection] = useState("Connecting to OpenCode…");
  const [busy, setBusy] = useState(false);
  const pending = useRef<{ id: string; text: string } | null>(null);
  const creating = useRef<{ id: string; text: string } | null>(null);
  const intakes = Object.values(workspace.intakes ?? {}).filter(i => i.owner === client.identity.owner);
  const intake = intakes.find(i => i.id === selected);
  useEffect(() => {
    const changed = () => { if (location.hash === "#intake") select(""); else if (location.hash.startsWith("#intake-")) select(location.hash.slice(1)); };
    window.addEventListener("hashchange", changed);
    return () => window.removeEventListener("hashchange", changed);
  }, []);
  useEffect(() => {
    setConversation(null);
    if (!intake) return;
    setConnection("Connecting to OpenCode…");
    const events = new EventSource(`${client.origin}/api/intakes/${encodeURIComponent(intake.id)}/events`);
    events.addEventListener("snapshot", event => { setConversation(JSON.parse(event.data)); setConnection("Connected to OpenCode"); });
    events.addEventListener("unavailable", event => setConnection(event.data));
    events.addEventListener("expired", () => { setError("Your session expired. Sign in again to continue."); events.close(); });
    events.onerror = () => setConnection("Reconnecting to OpenCode…");
    return () => events.close();
  }, [client, intake?.id]);
  async function run(action: () => Promise<unknown>) { setBusy(true); setError(""); try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); } }
  function open(item: Intake) { select(item.id); location.hash = item.id; setText(""); pending.current = null; }
  async function start() {
    if (!creating.current || creating.current.text !== description) creating.current = { id: `intake-${randomID()}`, text: description };
    select(creating.current.id); location.hash = creating.current.id;
    await client.intake(creating.current.id, description);
    setDescription(""); creating.current = null;
  }
  const action = (value: Record<string, unknown>) => client.intakeAction(intake!.id, value);
  return <section id="intake" className="intake-desk">
    <div className="section-heading"><h2>What would you like to work on?</h2>{intake && <button onClick={() => { select(""); location.hash = "intake"; }}>New conversation</button>}</div>
    <p className="muted">Describe the change. The agent will explore the code, ask what it needs to know, and draft the tickets.</p>
    {intakes.length > 0 && <label>Conversations<select value={selected} onChange={e => { const item = intakes.find(i => i.id === e.target.value); if (item) open(item); else {select(""); location.hash="intake";} }}><option value="">New conversation</option>{intakes.map(i => <option key={i.id} value={i.id}>{i.description.slice(0, 90)}</option>)}</select></label>}
    {!intake && <form onSubmit={e => { e.preventDefault(); void run(start); }}><label>Your idea<textarea aria-label="Your idea" rows={4} required maxLength={16384} placeholder="What should change, and why? Rough ideas are welcome." value={description} onChange={e => setDescription(e.target.value)}/></label><button className="primary" disabled={busy}>{busy ? "Starting…" : "Work through this"}</button></form>}
    {error && <p role="alert">{error}</p>}
    {intake && <div id={intake.id}>
      <div className="ticket-meta"><span className="badge">{intake.route}</span><span className="muted" role="status">{connection}</span></div>
      {intake.rationale && <p>{intake.rationale}</p>}
      <div className="conversation" aria-label="Intake conversation">
        {!conversation?.messages.length && <article className="chat-user"><strong>You</strong><p className="prose">{intake.description}</p></article>}
        {conversation?.messages.map(message => <article className={`chat-${message.role}`} key={message.id}><strong>{message.role === "user" ? "You" : "OpenCode"}</strong>{message.text && <p className="prose">{message.text}</p>}{message.parts?.map((part, index) => part.type === "text" ? <p className="prose" key={index}>{part.text}</p> : <p className="tool-status" key={index}>{part.name ?? "Tool"} · {part.status ?? "working"}</p>)}{message.error && <p role="alert">{message.error}</p>}</article>)}
      </div>
      {conversation?.forms.map(q => <QuestionForm key={q.id} question={q} busy={busy} reply={answer => void run(() => action({ action: "form", id: q.id, reply: { answer } }))}/>)}
      {conversation?.permissions.map(p => <article key={p.id}><h4>OpenCode needs permission</h4><p>{p.action}</p><pre>{p.resources.join("\n")}</pre><div className="actions"><button disabled={busy} onClick={() => void run(() => action({ action: "permission", id: p.id, reply: "once" }))}>Allow once</button><button disabled={busy} onClick={() => void run(() => action({ action: "permission", id: p.id, reply: "reject" }))}>Decline</button></div></article>)}
      <form onSubmit={e => { e.preventDefault(); void run(async () => { if (!pending.current || pending.current.text !== text) pending.current = { id: `msg_${randomID()}`, text }; await action({ action: "message", ...pending.current }); setText(""); pending.current = null; }); }}><label>Reply<textarea aria-label="Reply" rows={3} maxLength={16384} required value={text} onChange={e => setText(e.target.value)} placeholder="Answer a question or add more context…"/></label><div className="actions"><button className="primary" disabled={busy}>Send reply</button><button type="button" disabled={busy} onClick={() => void run(() => action({ action: "interrupt" }))}>Stop</button></div></form>
      {intake.tickets.length > 0 && <div className="draft-summary"><h3>Drafted tickets</h3>{intake.tickets.map(id => <p key={id}><a href={`#ticket-${id}`}>{workspace.tickets[id]?.title ?? id}</a> <span className="badge">{workspace.tickets[id]?.status ?? "removed"}</span></p>)}{intake.route === "implement" && intake.tickets.some(id => workspace.tickets[id]?.status === "draft" && !Object.values(workspace.tickets).some(t => t.parent === id)) && <button disabled={busy} className="primary" onClick={() => void run(() => action({ action: "ready", revision: intake.revision }))}>Mark implementation tickets ready</button>}</div>}
      <details><summary>OpenCode session and recovery</summary><p>The latest 100 messages appear here. Continue with the full history in OpenCode:</p><code>opencode --session {intake.conversation}</code><p><button disabled={busy} onClick={() => void run(() => action({ action: "resume" }))}>Reconnect session</button></p></details>
    </div>}
  </section>;
}
