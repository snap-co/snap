import { useState, useSyncExternalStore, type FormEvent } from "react";
import { startAuthy, type AuthyClient } from "../client";
import "./style.css";

function View({ client }: { client: AuthyClient }) {
  const state = useSyncExternalStore(client.subscribe, client.getSnapshot);
  const [register, setRegister] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const act = async (work: () => Promise<unknown>) => {
    setError(null);
    try { await work(); setPassword(""); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };
  const submit = (event: FormEvent) => { event.preventDefault(); void act(() => register ? client.createAccount(email, password) : client.signIn(email, password)); };
  return <main>
    <header><span className="brand">Snap / Authy</span><span className="connection" data-testid="connection">{state.connection}</span></header>
    {state.phase === "loading" ? <h1>Checking your session…</h1> : state.phase === "identified" ? <>
      <h1>You're signed in</h1>
      <p className="muted">Your session survives a page reload and server restart.</p>
      <p className="identity" data-testid="identity">{state.identityId}</p>
      <div className="actions"><button disabled={state.pending} onClick={() => void act(() => client.release({ scope: "current" }))}>Sign out</button><button className="secondary" onClick={() => void act(client.refresh)}>Refresh</button></div>
      <section><h2>Credentials</h2><ul>{state.credentials.map(c => <li key={c.credentialId}><strong>{c.label}</strong><span>{c.method}</span></li>)}</ul></section>
      <section><h2>Sessions</h2><p className="muted">Sign in from another browser to try remote sign-out.</p><ul>{state.sessions.map(s => <li key={s.sessionId}><div><strong>{s.current ? "This session" : "Another session"}</strong><small>Created {new Date(s.createdAt).toLocaleString()}</small></div>{!s.current && <button className="secondary" onClick={() => void act(() => client.release({ scope: "session", sessionId: s.sessionId }))}>Revoke session</button>}</li>)}</ul><button className="secondary" disabled={state.pending} onClick={() => void act(() => client.release({ scope: "others" }))}>Sign out other sessions</button></section>
    </> : <>
      <h1>{register ? "Create your account" : "Sign in"}</h1>
      <p className="muted">Password sessions, powered by the Rust client.</p>
      <form onSubmit={submit}>
        <label>Email<input name="email" type="email" autoComplete="username" required value={email} onChange={e => setEmail(e.target.value)} /></label>
        <label>Password<input name="password" type="password" autoComplete={register ? "new-password" : "current-password"} minLength={register ? 8 : undefined} maxLength={256} required value={password} onChange={e => setPassword(e.target.value)} /></label>
        <button disabled={state.pending}>{state.pending ? "Working…" : register ? "Create account" : "Sign in"}</button>
      </form>
      <button className="link" onClick={() => { setRegister(!register); setError(null); setPassword(""); }}>{register ? "Already have an account? Sign in" : "Create an account"}</button>
      {state.phase === "error" && <button className="secondary" onClick={() => void act(client.refresh)}>Retry connection</button>}
    </>}
    {(error ?? state.error?.failure?.message ?? state.error?.message) && <p role="alert">{error ?? state.error?.failure?.message ?? state.error?.message}</p>}
  </main>;
}

export default { start: startAuthy, View };
