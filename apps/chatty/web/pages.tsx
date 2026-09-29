import { useNavigate, useSearch } from "@tanstack/react-router";
import { useEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import "./style.css";
import { Chatty, randomID, type Thread, type View } from "../client";

export function App({ sdk }: { sdk: Chatty }) {
  const session = useSyncExternalStore(sdk.subscribe, sdk.getSnapshot).session;
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as { thread?: string };
  const [threads, setThreads] = useState<Thread[]>([]);
  const selected = search.thread ?? null;
  const [storedView, setView] = useState<View | null>(null);
  const view = storedView?.thread.id === selected ? storedView : null;
  const selection = useRef({ id: selected, generation: 0 });
  const [draft, setDraft] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [sidebar, setSidebar] = useState(false);
  const client = useRef<Chatty | null>(sdk);
  const end = useRef<HTMLDivElement>(null);
  const nearBottom = useRef(true);
  const pendingSend = useRef<{ thread: string; message: string; id: string } | null>(null);
  async function api<T>(path: string, body?: unknown): Promise<T> {
    if (!client.current) throw new Error("Chatty is still connecting");
    return client.current.command<T>(path, body ?? {});
  }
  useEffect(() => {
    const update = () => {
      const snapshot = sdk.getSnapshot();
      setThreads(snapshot.documents.map(d => ({ id: d.id, ...d.value })).sort((a,b) => b.updated - a.updated || a.id.localeCompare(b.id)));
      setView(selection.current.id ? sdk.view(selection.current.id) : null);
      if (snapshot.error) setError(snapshot.error);
    };
    const unsubscribe = sdk.subscribe(update);
    update();
    return unsubscribe;
  }, [sdk]);
  useEffect(() => {
    if (selection.current.id !== selected) selection.current = { id: selected, generation: selection.current.generation + 1 };
    setView(selected ? sdk.view(selected) : null);
    setError(""); setSidebar(false); nearBottom.current = true;
  }, [selected]);
  useEffect(() => { if (nearBottom.current) end.current?.scrollIntoView({ behavior: "instant" }); }, [view?.turns]);
  const choose = (id: string | null) => {
    if (selection.current.id === id) { setSidebar(false); return; }
    selection.current = { id, generation: selection.current.generation + 1 };
    setView(id ? client.current?.view(id) ?? null : null); setError(""); setSidebar(false); nearBottom.current = true;
    void navigate({ to: "/", search: id ? { thread: id } : {} });
  };
  async function action(work: () => Promise<void>) { setBusy(true); setError(""); try { await work(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); } finally { setBusy(false); } }
  const create = async () => {
    const chosen = selection.current;
    const thread = await api<{ id: string }>("chatty.create", { id: randomID(), title: "New thread", created: Math.floor(Date.now() / 1000) });
    if (selection.current === chosen) choose(thread.id); return thread.id;
  };
  async function editThread(thread: Thread, title: string, effort: string) {
    const chosen = selection.current;
    if (chosen.id !== thread.id) return;
    await client.current?.rename(thread.id, title, effort);
  }
  async function deleteThread() {
    const chosen = selection.current;
    if (!chosen.id) return;
    await api("chatty.delete", { thread_id: chosen.id });
    setThreads(t => t.filter(x => x.id !== chosen.id));
    if (selection.current === chosen) choose(null);
  }
  async function send(event: FormEvent) {
    event.preventDefault(); if (!draft.trim() || busy) return;
    await action(async () => {
      const thread = selected ?? await create();
      const chosen = selection.current;
      const message = draft.trim();
      if (!pendingSend.current || pendingSend.current.thread !== thread || pendingSend.current.message !== message) pendingSend.current = { thread, message, id: Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, "0")).join("") };
       await api("chatty.send", { thread_id: thread, message, request_id: pendingSend.current.id });
      pendingSend.current = null;
      if (selection.current === chosen && chosen.id === thread) {
        setDraft(current => current === draft ? "" : current); nearBottom.current = true;
        if (selection.current === chosen) setView(client.current?.view(thread) ?? null);
      }
    });
  }
  const logout = () => action(async () => {
    await client.current?.logout();
  });
  if (!session) return <main className="welcome"><div className="mark">c</div><h1>Chatty</h1><p>{error || "Opening your workspace…"}</p>{error && <button onClick={() => location.reload()}>Retry</button>}</main>;
  if (!session.identified) return <main className="welcome"><div className="mark">c</div><h1>Chatty</h1><p>Your conversations, synchronized across connected clients.</p><a className="primary" href="/auth/login">Continue with Authy <span>↗</span></a><small>Sign in or create an account at Authy.</small>{error && <p role="alert">{error}</p>}</main>;
  return <div className="app">
    <aside className={sidebar ? "open" : ""}>
      <header><span className="wordmark"><span className="mark">c</span>chatty</span><button className="icon mobile" aria-label="Close sidebar" onClick={() => setSidebar(false)}>×</button></header>
      <button className="new-thread" disabled={busy} onClick={() => void action(async () => { await create(); })}><span>＋</span> New conversation</button>
      <h2 className="eyebrow">YOUR CONVERSATIONS</h2>
      <nav>{threads.map(t => <button key={t.id} className={selected === t.id ? "selected" : ""} onClick={() => choose(t.id)}><span>{t.title}</span>{t.active_turn && <i aria-label="Reply in progress" />}</button>)}{!threads.length && <p className="muted empty-nav">Your first conversation starts here.</p>}</nav>
      <div className="account"><div className="avatar">{(session.account?.name || "You").slice(0, 1).toUpperCase()}</div><div><strong>{session.account?.name}</strong><small>{session.account?.email}</small></div><button title="Sign out" aria-label="Sign out" className="icon" disabled={busy} onClick={() => void logout()}>↗</button></div>
    </aside>
    {sidebar && <button className="scrim" aria-label="Close sidebar" onClick={() => setSidebar(false)} />}
    <main className="conversation">
      <header className="topbar"><button className="icon mobile" aria-label="Open sidebar" onClick={() => setSidebar(true)}>☰</button><div><strong>{view?.thread.title ?? "New conversation"}</strong><small>Connected conversation</small></div>{selected && <div className="thread-actions"><button className="quiet" disabled={busy || !view} onClick={() => { if (!view) return; const title = prompt("Conversation title", view.thread.title); if (title) void action(() => editThread(view.thread, title, view.thread.effort)); }}>Rename</button><button className="quiet" disabled={busy} onClick={() => { if (confirm("Delete this conversation from active views?")) void action(deleteThread); }}>Delete</button></div>}</header>
      <div className="messages" onScroll={e => { const node = e.currentTarget; nearBottom.current = node.scrollHeight - node.scrollTop - node.clientHeight < 140; }}>
        {!view?.turns.length && <section className="empty"><div className="spark">✳</div><h1>What's on your mind?</h1><p>Work through a question, explore an idea, or make something worth keeping.</p><div className="suggestions">{["Help me think through a decision", "Create a plan for my week", "Find a useful starting point"].map(text => <button key={text} onClick={() => setDraft(text)}>{text}<span>↗</span></button>)}</div></section>}
        <div className="transcript">{view?.turns.map(turn => <article key={turn.id}>
          <div className="user-message"><span className="label">YOU</span><p>{turn.user}</p></div>
          {(turn.text || turn.summary || turn.tools.length > 0 || turn.error) && <div className="assistant-message"><span className="label">SAVED REPLY</span>
            {turn.summary && <details className="reasoning"><summary>Reasoning summary</summary><div className="prose"><Text value={turn.summary} /></div></details>}
            {turn.tools.map(tool => <details className="tool" key={tool.call_id}><summary>{tool.name.replaceAll("_", " ")} <span>{tool.status === "running" ? "Running…" : "Finished"}</span></summary><pre>{JSON.stringify({ arguments: tool.arguments, result: tool.result }, null, 2)}</pre></details>)}
            <div className="prose"><Text value={turn.text} /></div>
            {turn.error && <p className="turn-error" role="status">{turn.error}</p>}
            {!!turn.usage.output_tokens && <small className="usage">{turn.usage.input_tokens?.toLocaleString()} input · {turn.usage.output_tokens.toLocaleString()} output{turn.usage.reasoning_tokens ? ` · ${turn.usage.reasoning_tokens.toLocaleString()} reasoning` : ""}</small>}
            {!!turn.usage.context_omitted && <small className="usage">{turn.usage.context_omitted} earlier turns were omitted from this request's context.</small>}
          </div>}
        </article>)}<div ref={end} /></div>
      </div>
      <div className="composer-area">{error && <div className="error" role="alert">{error}<button className="icon" aria-label="Dismiss error" onClick={() => setError("")}>×</button></div>}
        <form className="composer" onSubmit={e => void send(e)}><textarea aria-label="Message Chatty" placeholder="Write a message…" value={draft} maxLength={32768} onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); e.currentTarget.form?.requestSubmit(); } }} /><div className="composer-bottom"><span className="capabilities">Messages synchronize across your clients</span><button className="send" type="submit" aria-label="Send message" disabled={!draft.trim() || busy}>↑</button></div></form>
      </div>
    </main>
  </div>;
}

/** React escapes source text; only explicit HTTP(S) URLs become navigable links. */
function Text({ value }: { value: string }) {
  return <>{value.split(/(```[\s\S]*?```)/g).map((part, i) => part.startsWith("```") ? <pre key={i}><code>{part.replace(/^```[^\n]*\n?/, "").replace(/```$/, "")}</code></pre> : part.split(/\n\n+/).filter(Boolean).map((paragraph, j) => <p key={`${i}-${j}`}>{paragraph.split(/(\[[^\]]+\]\(https?:\/\/[^\s)]+\)|https?:\/\/[^\s<>]+)/g).map((piece, k) => {
    const link = piece.match(/^\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)$/);
    if (link) return <a key={k} href={link[2]} target="_blank" rel="noopener noreferrer">{link[1]}</a>;
    if (/^https?:\/\//.test(piece)) { const url = piece.replace(/[.,;!?)]+$/, ""); return <span key={k}><a href={url} target="_blank" rel="noopener noreferrer">{url}</a>{piece.slice(url.length)}</span>; }
    return piece;
  })}</p>))}</>;
}
