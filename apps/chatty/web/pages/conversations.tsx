import { useNavigate, useSearch } from "@tanstack/react-router";
import { useEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import { Chatty, randomID, type Thread, type View } from "../client";
import { Text } from "./text";

export function ConversationsPage({ sdk }: { sdk: Chatty }) {
  const snapshot = useSyncExternalStore(sdk.subscribe, sdk.getSnapshot);
  const session = snapshot.session;
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
      setThreads(snapshot.threads);
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
  useEffect(() => { if (snapshot.ready && snapshot.connected) sdk.select(selected); }, [selected, snapshot.ready, snapshot.connected, sdk]);
  useEffect(() => { if (nearBottom.current) end.current?.scrollIntoView({ behavior: "instant" }); }, [view?.messages]);
  const choose = (id: string | null) => {
    if (selection.current.id === id) { setSidebar(false); return; }
    selection.current = { id, generation: selection.current.generation + 1 };
    setView(id ? client.current?.view(id) ?? null : null); setError(""); setSidebar(false); nearBottom.current = true;
    void navigate({ to: "/", search: id ? { thread: id } : {} });
  };
  async function action(work: () => Promise<void>) { setBusy(true); setError(""); try { await work(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); } finally { setBusy(false); } }
  const create = async () => {
    const chosen = selection.current;
    const thread = await api<{ id: string }>("chatty.create", { id: randomID(), title: "New thread" });
    if (selection.current === chosen) choose(thread.id); return thread.id;
  };
  async function editThread(thread: Thread, title: string) {
    const chosen = selection.current;
    if (chosen.id !== thread.id) return;
    await client.current?.rename(thread.id, title);
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
  return <div className="app">
    <aside className={sidebar ? "open" : ""}>
      <header><span className="wordmark"><span className="mark">c</span>chatty</span><button className="icon mobile" aria-label="Close sidebar" onClick={() => setSidebar(false)}>×</button></header>
      <button className="new-thread" disabled={busy} onClick={() => void action(async () => { await create(); })}><span>＋</span> New conversation</button>
      <nav aria-label="Threads">{threads.map(t => <button key={t.id} className={selected === t.id ? "selected" : ""} onClick={() => choose(t.id)}><span>{t.title}</span></button>)}{!threads.length && <p className="muted empty-nav">Create a thread to start a conversation.</p>}</nav>
      <div className="account"><div className="avatar">{(session.account?.name || "You").slice(0, 1).toUpperCase()}</div><div><strong>{session.account?.name}</strong><small>{session.account?.email}</small></div><button title="Sign out" aria-label="Sign out" className="icon" disabled={busy} onClick={() => void logout()}>↗</button></div>
    </aside>
    {sidebar && <button className="scrim" aria-label="Close sidebar" onClick={() => setSidebar(false)} />}
    <main className="conversation">
      <header className="topbar shared-thread"><button className="icon mobile" aria-label="Open sidebar" onClick={() => setSidebar(true)}>☰</button><div><strong>{view?.thread.title ?? "New conversation"}</strong><small>{snapshot.connected ? "Shared thread" : "Reconnecting"}</small></div>{selected && <div className="thread-actions"><button className="quiet" disabled={busy || !view} onClick={() => { const identity = prompt("Chatty identity to add as a member"); if (identity) void action(async () => { await api("chatty.member", { thread_id:selected, identity, role:"editor" }); }); }}>Add member</button><button className="quiet" disabled={busy || !view} onClick={() => { const identity = prompt("Chatty identity to remove"); if (identity) void action(async () => { await api("chatty.member", { thread_id:selected, identity, role:null }); }); }}>Remove member</button><button className="quiet" disabled={busy || !view} onClick={() => { if (!view) return; const title = prompt("Conversation title", view.thread.title); if (title) void action(() => editThread(view.thread, title)); }}>Rename</button><button className="quiet" disabled={busy} onClick={() => { if (confirm("Permanently delete this thread and all its messages?")) void action(deleteThread); }}>Delete</button></div>}</header>
      <div className="messages" onScroll={e => { const node = e.currentTarget; nearBottom.current = node.scrollHeight - node.scrollTop - node.clientHeight < 140; }}>
        {!view?.messages.length && <section className="empty"><h1>{selected ? "Start the conversation" : "A place to talk"}</h1><p>People and agents post here as members. Add a member to share a thread.</p></section>}
        <div className="transcript">{view?.messages.map(message => <article key={message.sequence} className={message.sender === session.account?.owner ? "own-message" : "peer-message"}>
          <div className="user-message"><span className="label" title={message.sender}>{message.sender === session.account?.owner ? "You" : message.sender.slice(0,12)}</span><div className="prose"><Text value={message.body} /></div></div>
        </article>)}<div ref={end} /></div>
      </div>
      <div className="composer-area">{error && <div className="error" role="alert">{error}<button className="icon" aria-label="Dismiss error" onClick={() => setError("")}>×</button></div>}
        <form className="composer" onSubmit={e => void send(e)}><textarea aria-label="Message Chatty" placeholder="Write a message…" value={draft} maxLength={32768} onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); e.currentTarget.form?.requestSubmit(); } }} /><div className="composer-bottom"><span className="capabilities">Messages synchronize across your clients</span><button className="send" type="submit" aria-label="Send message" disabled={!draft.trim() || busy}>↑</button></div></form>
      </div>
    </main>
  </div>;
}
