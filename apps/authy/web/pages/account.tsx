import { useEffect, useState, useSyncExternalStore, type FormEvent } from "react";
import { Link } from "@tanstack/react-router";
import { AuthyClient } from "../../client";
import { AuthShell, AuthHeading } from "../auth-ui";

function accountError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (/connecting|Disconnected/i.test(message)) return "Authy is reconnecting. Wait for the connection, then try again.";
  if (/Session check failed/i.test(message)) return "We couldn't check your session. Check your connection and reload the page.";
  if (/Session expired|InvalidBearer|Account session ended/i.test(message)) return "Your session has ended. Sign in again to continue.";
  if (/Edit rejected|Profile out of sync|Profile is still loading|Sign in before saving/i.test(message)) return message;
  return "We couldn't complete that action. Check your connection and try again. If it keeps happening, reload the page.";
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

export function AccountPage({ client }: { client: AuthyClient }) {
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
    <AuthShell brand={<Link className="brand" to="/" aria-label="Authy home">Snap <span className="brand-divider">/</span> Authy</Link>} status={snapshot.account && <span className="connection" data-testid="connection">
          {connectionLabel}
        </span>}>
      <AuthHeading title="You're signed in">Manage your profile and active sessions.</AuthHeading>
      <p className="identity" data-testid="account-email">
        {snapshot.account?.email}
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
      {(actionError ?? snapshot.error) && (
        <p role="alert">{actionError ?? accountError(snapshot.error)}</p>
      )}
    </AuthShell>
  );
}
