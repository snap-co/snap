import { createRoot } from "react-dom/client";
import { useEffect, useRef, useState, type FormEvent } from "react";
import "./style.css";

type Session = { identified: boolean; csrf?: string; account?: { id: string; name: string; email: string }; model: string; model_ready: boolean; files_available: boolean; search_available: boolean };
type Thread = { id: string; title: string; effort: string; active_turn: string; updated: number };
type Turn = { id: string; user: string; text: string; summary: string; status: string; error: string; tools: { call_id: string; name: string; arguments: unknown; status: string; result?: unknown }[]; usage: { input_tokens?: number; output_tokens?: number; reasoning_tokens?: number; context_omitted?: number } };
type View = { thread: Thread; turns: Turn[] };

function App() {
  const [session, setSession] = useState<Session | null>(null);
  const [threads, setThreads] = useState<Thread[]>([]);
  const [selected, setSelected] = useState<string | null>(new URLSearchParams(location.search).get("thread"));
  const [storedView, setView] = useState<View | null>(null);
  const view = storedView?.thread.id === selected ? storedView : null;
  const selection = useRef({ id: selected, generation: 0 });
  const [draft, setDraft] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [sidebar, setSidebar] = useState(false);
  const [effort, setEffort] = useState("medium");
  const sessionRef = useRef(session); sessionRef.current = session;
  const end = useRef<HTMLDivElement>(null);
  const nearBottom = useRef(true);
  const pendingSend = useRef<{ thread: string; message: string; id: string } | null>(null);
  async function api<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    const response = await fetch(path, { signal, ...(body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json", "x-chatty-csrf": sessionRef.current?.csrf ?? "" }, body: JSON.stringify(body) }) });
    const data = await response.json();
    if (!response.ok) {
      if (response.status === 401) setSession(s => s && ({ ...s, identified: false }));
      throw new Error(data.error_description ?? "Request failed");
    }
    return data as T;
  }
  useEffect(() => {
    const controller = new AbortController();
    api<Session>("/api/session", undefined, controller.signal).then(setSession).catch(e => { if (!controller.signal.aborted) setError(String(e)); });
    return () => controller.abort();
  }, []);
  useEffect(() => {
    if (!session?.identified) { setThreads([]); setView(null); return; }
    const chosen = selection.current;
    const controller = new AbortController(); let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const list = await api<{ threads: Thread[] }>("/api/threads", undefined, controller.signal);
        if (controller.signal.aborted) return;
        setThreads(list.threads);
        if (selected) {
          const next = await api<View>(`/api/thread?id=${encodeURIComponent(selected)}`, undefined, controller.signal);
          if (!controller.signal.aborted && selection.current === chosen) setView(next);
        }
      } catch (e) { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e)); }
      finally { if (!controller.signal.aborted) timer = setTimeout(poll, 700); }
    };
    void poll();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [session?.identified, selected]);
  useEffect(() => { if (nearBottom.current) end.current?.scrollIntoView({ behavior: "instant" }); }, [view?.turns]);
  const choose = (id: string | null) => {
    if (selection.current.id === id) { setSidebar(false); return; }
    selection.current = { id, generation: selection.current.generation + 1 };
    setSelected(id); setView(null); setError(""); setSidebar(false); nearBottom.current = true;
    history.replaceState(null, "", id ? `/?thread=${encodeURIComponent(id)}` : "/");
  };
  async function action(work: () => Promise<void>) { setBusy(true); setError(""); try { await work(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); } finally { setBusy(false); } }
  const create = async () => {
    const chosen = selection.current;
    const thread = await api<Thread>("/api/thread/create", { effort });
    setThreads(t => [thread, ...t]); if (selection.current === chosen) choose(thread.id); return thread.id;
  };
  async function editThread(thread: Thread, title: string, effort: string) {
    const chosen = selection.current;
    if (chosen.id !== thread.id) return;
    const next = await api<View>("/api/thread/rename", { thread_id: thread.id, title, effort });
    if (selection.current === chosen) setView(next);
  }
  async function deleteThread() {
    const chosen = selection.current;
    if (!chosen.id) return;
    await api("/api/thread/delete", { thread_id: chosen.id });
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
      await api("/api/send", { thread_id: thread, message, request_id: pendingSend.current.id });
      pendingSend.current = null;
      if (selection.current === chosen && chosen.id === thread) {
        setDraft(current => current === draft ? "" : current); nearBottom.current = true;
        const next = await api<View>(`/api/thread?id=${encodeURIComponent(thread)}`);
        if (selection.current === chosen) setView(next);
      }
    });
  }
  const logout = () => action(async () => {
    const result = await api<{ redirect: string }>("/auth/logout", {});
    location.assign(result.redirect);
  });
  if (!session) return <main className="welcome"><div className="mark">c</div><h1>Chatty</h1><p>{error || "Opening your workspace…"}</p>{error && <button onClick={() => location.reload()}>Retry</button>}</main>;
  if (!session.identified) return <main className="welcome"><div className="mark">c</div><span className="eyebrow">YOUR PERSONAL ASSISTANT</span><h1>A place to think<br />things through.</h1><p>Conversations that stay with you. A private workspace for notes, questions, and the next idea.</p><a className="primary" href="/auth/login">Continue with Authy <span>↗</span></a><small>Sign in or create an account at Authy.</small>{error && <p role="alert">{error}</p>}<footer>CHATTY · POWERED BY MUSE SPARK</footer></main>;
  const active = view?.thread.active_turn;
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
      <header className="topbar"><button className="icon mobile" aria-label="Open sidebar" onClick={() => setSidebar(true)}>☰</button><div><strong>{view?.thread.title ?? "New conversation"}</strong><small>Muse Spark <span>Contributor</span></small></div>{selected && <div className="thread-actions"><button className="quiet" disabled={busy || !view} onClick={() => { if (!view) return; const title = prompt("Conversation title", view.thread.title); if (title) void action(() => editThread(view.thread, title, view.thread.effort)); }}>Rename</button><button className="quiet" disabled={busy} onClick={() => { if (confirm("Delete this conversation and its messages?")) void action(deleteThread); }}>Delete</button></div>}</header>
      <div className="messages" onScroll={e => { const node = e.currentTarget; nearBottom.current = node.scrollHeight - node.scrollTop - node.clientHeight < 140; }}>
        {!view?.turns.length && <section className="empty"><div className="spark">✳</div><h1>What's on your mind?</h1><p>Work through a question, explore an idea, or make something worth keeping.</p><div className="suggestions">{["Help me think through a decision", "Create a plan for my week", "Find a useful starting point"].map(text => <button key={text} onClick={() => setDraft(text)}>{text}<span>↗</span></button>)}</div></section>}
        <div className="transcript">{view?.turns.map(turn => <article key={turn.id}>
          <div className="user-message"><span className="label">YOU</span><p>{turn.user}</p></div>
          <div className="assistant-message"><span className="label"><span className="mini-spark">✳</span> CHATTY {turn.status === "running" && <span className="working">Working…</span>}</span>
            {turn.summary && <details className="reasoning"><summary>Reasoning summary</summary><div className="prose"><Text value={turn.summary} /></div></details>}
            {turn.tools.map(tool => <details className="tool" key={tool.call_id}><summary>{tool.name.replaceAll("_", " ")} <span>{tool.status === "running" ? "Running…" : "Finished"}</span></summary><pre>{JSON.stringify({ arguments: tool.arguments, result: tool.result }, null, 2)}</pre></details>)}
            <div className="prose"><Text value={turn.text} /></div>
            {turn.error && <p className="turn-error" role="status">{turn.error}</p>}
            {!!turn.usage.output_tokens && <small className="usage">{turn.usage.input_tokens?.toLocaleString()} input · {turn.usage.output_tokens.toLocaleString()} output{turn.usage.reasoning_tokens ? ` · ${turn.usage.reasoning_tokens.toLocaleString()} reasoning` : ""}</small>}
            {!!turn.usage.context_omitted && <small className="usage">{turn.usage.context_omitted} earlier turns were omitted from this request's context.</small>}
          </div>
        </article>)}<div ref={end} /></div>
      </div>
      <div className="composer-area">{error && <div className="error" role="alert">{error}<button className="icon" aria-label="Dismiss error" onClick={() => setError("")}>×</button></div>}
        <form className="composer" onSubmit={e => void send(e)}><textarea aria-label="Message Chatty" placeholder="Ask anything, or pick up where you left off…" value={draft} maxLength={32768} onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); e.currentTarget.form?.requestSubmit(); } }} /><div className="composer-bottom"><label className="effort">Thinking <select aria-label="Thinking effort" value={view?.thread.effort ?? effort} disabled={!!active || busy || !!selected && !view} onChange={e => { const value = e.target.value; setEffort(value); if (view) void action(() => editThread(view.thread, view.thread.title, value)); }}>{["minimal", "low", "medium", "high", "xhigh"].map(v => <option key={v} value={v}>{v === "xhigh" ? "Extra high" : v[0].toUpperCase() + v.slice(1)}</option>)}</select></label><span className="capabilities">{session.files_available && "Files"}{session.files_available && session.search_available && " · "}{session.search_available && "Web search"}</span>{active ? <button type="button" className="send" aria-label="Stop reply" onClick={() => void action(async () => { await api("/api/cancel", { thread_id: selected, turn_id: active }); })}>■</button> : <button className="send" type="submit" aria-label="Send message" disabled={!draft.trim() || busy || !session.model_ready}>↑</button>}</div></form>
        <p className="composer-note">{session.model_ready ? "Muse Spark Contributor · Check important information." : "Set OPENCODE_API_KEY on the server to enable replies."}</p>
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
createRoot(document.getElementById("root")!).render(<App />);
