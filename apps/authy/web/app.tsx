import { useEffect, useState, useSyncExternalStore, type FormEvent } from "react";
import { startAuthy, type AuthyClient } from "../client";
import "./style.css";

function View({ client }: { client: AuthyClient }) {
  const state = useSyncExternalStore(client.subscribe, client.getSnapshot);
  const [register, setRegister] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const params = new URLSearchParams(location.search);
  const candidate = params.get("return_to");
  const returnTo = candidate?.startsWith("/oauth/resume?") || candidate?.startsWith("/oauth/authorize?") ? candidate : null;
  const [reauthenticated, setReauthenticated] = useState(params.get("reauth") !== "1");
  useEffect(() => {
    if (state.phase === "anonymous") setReauthenticated(true);
    if (state.phase === "identified" && returnTo && reauthenticated) location.replace(returnTo);
  }, [state.phase, returnTo, reauthenticated]);
  const act = async (work: () => Promise<unknown>) => {
    setError(null);
    try { await work(); setPassword(""); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };
  const submit = (event: FormEvent) => { event.preventDefault(); void act(() => register ? client.createAccount(email, password) : client.signIn(email, password)); };
  return <main>
    <header><span className="brand">Snap / Authy</span><span className="connection" data-testid="connection">{state.connection}</span></header>
    {state.phase === "loading" ? <h1>Checking your session…</h1> : state.phase === "identified" ? <>
      <h1>You're signed in</h1>
      {returnTo && !reauthenticated ? <section><h2>Confirm your identity</h2><p>The app requested a fresh sign-in. Sign out here, then enter the account you want to use.</p><button onClick={() => void act(() => client.release({ scope: "current" }))}>Sign in again</button></section> : null}
      <p className="muted">Your session survives a page reload and server restart.</p>
      <p className="identity" data-testid="identity">{state.identityId}</p>
      <Profile identity={state.identityId!} />
      <div className="actions"><button disabled={state.pending} onClick={() => void act(() => client.release({ scope: "current" }))}>Sign out</button><button className="secondary" onClick={() => void act(client.refresh)}>Refresh</button></div>
      <section><h2>Credentials</h2><ul>{state.credentials.map(c => <li key={c.credentialId}><strong>{c.label}</strong><span>{c.method}</span></li>)}</ul></section>
      <section><h2>Sessions</h2><p className="muted">Sign in from another browser to try remote sign-out.</p><ul>{state.sessions.map(s => <li key={s.sessionId}><div><strong>{s.current ? "This session" : "Another session"}</strong><small>Created {new Date(s.createdAt).toLocaleString()}</small></div>{!s.current && <button className="secondary" onClick={() => void act(() => client.release({ scope: "session", sessionId: s.sessionId }))}>Revoke session</button>}</li>)}</ul><button className="secondary" disabled={state.pending} onClick={() => void act(() => client.release({ scope: "others" }))}>Sign out other sessions</button></section>
    </> : <>
      <h1>{register ? "Create your account" : "Sign in"}</h1>
      <p className="muted">{returnTo ? "Sign in or create an Authy account to continue to your app." : "One account for your Snap apps."}</p>
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

type Account = { id: string; email: string; name: string; bio: string; revision: number };
function Profile({ identity }: { identity: string }) {
  const [account, setAccount] = useState<Account | null>(null);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [pending, setPending] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    fetch("/api/account", { signal: controller.signal }).then(async r => {
      if (!r.ok) throw new Error("Could not load account");
      setAccount(await r.json() as Account);
    }).catch(e => { if (!controller.signal.aborted) setError(String(e)); });
    return () => controller.abort();
  }, [identity]);
  async function save(event: FormEvent) {
    event.preventDefault(); setPending(true); setSaved(false); setError("");
    try {
      const response = await fetch("/api/account", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(account) });
      const value = await response.json();
      if (!response.ok) throw new Error(value.error_description ?? "Could not save account");
      setAccount(value as Account); setSaved(true);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setPending(false); }
  }
  return <section><h2>Your account</h2>{account && <form onSubmit={e => void save(e)}>
    <label>Display name<input value={account.name} maxLength={100} required onChange={e => { setAccount({ ...account, name: e.target.value }); setSaved(false); }} /></label>
    <label>About you<textarea value={account.bio} maxLength={2000} onChange={e => { setAccount({ ...account, bio: e.target.value }); setSaved(false); }} /></label>
    <p className="muted">{account.email} · Email not yet verified</p>
    <button disabled={pending}>{pending ? "Saving…" : "Save account"}</button>{saved && <p role="status">Account saved</p>}
  </form>}{error && <p role="alert">{error}</p>}</section>;
}

export default { start: startAuthy, View };
