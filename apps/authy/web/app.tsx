import { useEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import { createRoot } from "react-dom/client";
import { startAuthy, type AuthyClient } from "../client";
import "./style.css";
import { AuthShell, AuthHeading, AuthActions, AuthButton, AuthField } from "./auth-ui";

function accountError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (/connecting|Disconnected/i.test(message)) return "Authy is reconnecting. Wait for the connection, then try again.";
  if (/Session check failed/i.test(message)) return "We couldn't check your session. Check your connection and reload the page.";
  if (/Session expired|InvalidBearer|Account session ended/i.test(message)) return "Your session has ended. Sign in again to continue.";
  if (/Edit rejected|Profile out of sync|Profile is still loading|Sign in before saving/i.test(message)) return message;
  return "We couldn't complete that action. Check your connection and try again. If it keeps happening, reload the page.";
}

function AuthForm({ client }: { client: AuthyClient }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [mode, setMode] = useState<"signin" | "signup">("signin");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submitting = useRef(false);
  const [visible, setVisible] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting.current) return;
    setError(null);
    const bytes = new TextEncoder().encode(password).length;
    if (mode === "signup" && (bytes < 8 || bytes > 1024)) {
      setError(bytes < 8 ? "Choose a longer password: use at least 8 bytes." : "Your password is too long. Use no more than 1,024 bytes.");
      return;
    }
    submitting.current = true;
    setBusy(true);
    try {
      await (mode === "signup" ? client.signup(email, password) : client.login(email, password));
      setPassword("");
    } catch (e) {
      setError(e instanceof Error ? e.message : "We couldn't sign you in. Check your connection and try again.");
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };

  return (
    <>
      <AuthHeading title={mode === "signup" ? "Create your account" : "Sign in"}>One account for your Snap apps.</AuthHeading>
      <form onSubmit={submit} className="signin-form" aria-busy={busy}>
        <AuthField label="Email" id="email">
        <input
          id="email"
          name="email"
          type="email"
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          readOnly={busy}
          required
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        </AuthField>
        <AuthField label="Password" id="password">
        <div className="password-control">
        <input
          id="password"
          name="password"
          type={visible ? "text" : "password"}
          autoComplete={mode === "signup" ? "new-password" : "current-password"}
          aria-describedby={mode === "signup" ? "password-hint" : undefined}
          readOnly={busy}
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <AuthButton type="button" secondary aria-controls="password" aria-label={visible ? "Hide password" : "Show password"} onClick={() => setVisible(!visible)}>{visible ? "Hide" : "Show"}</AuthButton>
        </div>
        {mode === "signup" && <small id="password-hint">Use a unique password, 8–1,024 bytes. A typical letter or number uses one byte.</small>}
        </AuthField>
        <AuthActions>
          <AuthButton type="submit" disabled={busy}>
            {busy ? (mode === "signup" ? "Creating account…" : "Signing in…") : (mode === "signup" ? "Create account" : "Sign in")}
          </AuthButton>
          <AuthButton
            type="button"
            secondary
            className="mode-switch"
            disabled={busy}
            onClick={() => {
              setMode(mode === "signup" ? "signin" : "signup");
              setError(null);
              setPassword("");
              setVisible(false);
            }}
          >
            {mode === "signup" ? "Have an account? Sign in" : "New here? Create account"}
          </AuthButton>
        </AuthActions>
      </form>
      <p className="submission-status" role="status">{busy ? (mode === "signup" ? "Creating your account…" : "Checking your sign-in details…") : ""}</p>
      {error && <p role="alert">{error}</p>}
    </>
  );
}

function ProfileEditor({ client }: { client: AuthyClient }) {
  const snapshot = useSyncExternalStore(client.subscribe, client.getSnapshot);
  const server = snapshot.profile;
  const [name, setName] = useState(server?.name ?? "");
  const [bio, setBio] = useState(server?.bio ?? "");
  const [lastSyncedRevision, setLastSyncedRevision] = useState<string | null>(
    server ? server.revision : null,
  );

  // Adopt the authoritative view when it first loads or when a new revision
  // arrives that this editor did not just optimistically project. After an
  // "edit rejected: profile changed" error the current view stays on screen,
  // so the user can review it and retry the save.
  useEffect(() => {
    if (!server) return;
    if (lastSyncedRevision === null || server.revision !== lastSyncedRevision) {
      setName(server.name);
      setBio(server.bio);
      setLastSyncedRevision(server.revision);
    }
  }, [server, lastSyncedRevision]);

  const save = (event: FormEvent) => {
    event.preventDefault();
    client.saveProfile(name, bio);
  };

  return (
    <section>
      <h2>Your profile</h2>
      {server ? (
        <form onSubmit={save}>
          <label htmlFor="profile-name">Name</label>
          <input
            id="profile-name"
            name="name"
            value={name}
            maxLength={100}
            required
            onChange={(e) => setName(e.target.value)}
          />
          <label htmlFor="profile-bio">Bio</label>
          <textarea
            id="profile-bio"
            name="bio"
            value={bio}
            maxLength={2000}
            onChange={(e) => setBio(e.target.value)}
          />
          <p className="muted">
            {snapshot.pending > 0 ? "Projected revision " : "Saved revision "}{server.revision}
            {snapshot.pending > 0 ? ` · ${snapshot.pending} save(s) in flight` : ""}
          </p>
          <div className="actions">
            <button type="submit" disabled={snapshot.saving || snapshot.connection !== "connected"}>
              {snapshot.saving ? "Saving…" : "Save profile"}
            </button>
            <button
              type="button"
              className="secondary"
              onClick={() => {
                setName(server.name);
                setBio(server.bio);
              }}
            >
              Reload current values
            </button>
          </div>
        </form>
      ) : (
        <p className="muted">
          {snapshot.connection === "connected"
            ? "Loading your profile…"
            : "Connect to load your profile."}
        </p>
      )}
      {snapshot.saving && <p role="status">Saving your profile…</p>}
    </section>
  );
}

function Sessions({ client }: { client: AuthyClient }) {
  const snapshot = useSyncExternalStore(client.subscribe, client.getSnapshot);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (snapshot.account && snapshot.sessions === null) {
      void client.refreshSessions().catch((e) => setError(accountError(e)));
    }
  }, [client, snapshot.account, snapshot.sessions]);

  const act = async (work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(accountError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section>
      <h2>Sessions</h2>
      {snapshot.credentials && snapshot.credentials.length > 0 && (
        <p className="muted">
          Signed in with {snapshot.credentials.map((c) => c.label).join(", ")} ({snapshot.credentials[0].kind}).
        </p>
      )}
      {snapshot.sessions === null ? (
        <p className="muted">Loading sessions…</p>
      ) : snapshot.sessions.length === 0 ? (
        <p className="muted">No active sessions.</p>
      ) : (
        <ul>
          {snapshot.sessions.map((session) => (
            <li key={session.id}>
              <div>
                <strong>{session.current ? "This session" : "Another session"}</strong>
                <small>Expires {new Date(session.expires * 1000).toLocaleString()}</small>
              </div>
            </li>
          ))}
        </ul>
      )}
      <div className="actions">
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void act(() => client.refreshSessions())}
        >
          Refresh sessions
        </button>
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void act(() => client.logout("others"))}
        >
          Sign out other sessions
        </button>
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void act(() => client.logout("all"))}
        >
          Sign out everywhere
        </button>
      </div>
      {error && <p role="alert">{error}</p>}
    </section>
  );
}

export function View({ client }: { client: AuthyClient }) {
  const snapshot = useSyncExternalStore(client.subscribe, client.getSnapshot);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const act = async (work: () => Promise<unknown>) => {
    setBusy(true);
    setActionError(null);
    try {
      await work();
    } catch (e) {
      setActionError(accountError(e));
    } finally {
      setBusy(false);
    }
  };

  const connectionLabel =
    snapshot.connection === "connected"
      ? "Account connected"
      : snapshot.connection === "connecting"
        ? "Connecting…"
         : "Reconnecting…";

  return (
    <AuthShell status={snapshot.account && <span className="connection" data-testid="connection">
          {connectionLabel}
        </span>}>
      {snapshot.phase === "loading" ? (
        <AuthHeading title="Checking your session…" />
      ) : snapshot.phase === "anonymous" || !snapshot.account ? (
        <AuthForm client={client} />
      ) : (
        <>
          <AuthHeading title="You're signed in">Manage your profile and active sessions.</AuthHeading>
          <p className="identity" data-testid="account-email">
            {snapshot.account.email}
          </p>
          <ProfileEditor client={client} />
          <Sessions client={client} />
          <div className="actions">
            <button disabled={busy} onClick={() => void act(() => client.logout())}>
              Sign out
            </button>
            {snapshot.connection !== "connected" && (
              <button className="secondary" onClick={() => client.retryConnection()}>
                Retry connection
              </button>
            )}
          </div>
        </>
      )}
      {(actionError ?? snapshot.error) && (
        <p role="alert">{actionError ?? accountError(snapshot.error)}</p>
      )}
    </AuthShell>
  );
}

function App() {
  const [client, setClient] = useState<AuthyClient | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    let instance: AuthyClient | null = null;
    startAuthy()
      .then((started) => {
        if (!live) {
          started.close();
          return;
        }
        instance = started;
        setClient(started);
      })
      .catch((e) => {
        if (live) setFailed(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
      instance?.close();
    };
  }, []);
  if (failed) {
    return (
      <AuthShell>
        <AuthHeading title="Authy is unavailable" />
         <p role="alert">Authy couldn't load. Check your connection and reload the page.</p>
         <AuthButton onClick={() => location.reload()}>Reload Authy</AuthButton>
      </AuthShell>
    );
  }
  if (!client) {
    return (
      <AuthShell><AuthHeading title="Checking your session…" /></AuthShell>
    );
  }
  return <View client={client} />;
}

createRoot(document.getElementById("root")!).render(<App />);
