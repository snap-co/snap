import { useEffect, useState, useSyncExternalStore, type FormEvent } from "react";
import { Link } from "@tanstack/react-router";
import { AuthyClient } from "../client";
import { AuthShell, AuthHeading } from "../auth-ui";

function accountError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (/connecting|Disconnected/i.test(message)) return "Authy is reconnecting. Wait for the connection, then try again.";
  if (/Session check failed/i.test(message)) return "We couldn't check your session. Check your connection and reload the page.";
  if (/Session expired|InvalidBearer|Account session ended/i.test(message)) return "Your session has ended. Sign in again to continue.";
  if (/Conflict/.test(message)) return "Your profile changed in another session. Review the current values and save again.";
  if (/Denied/.test(message)) return "You no longer have access to this profile.";
  if (/Invalid/.test(message)) return "Enter a first name and keep each name under 101 characters.";
  if (/outcome unknown/.test(message)) return "The connection ended before the save completed. Check the current values before saving again.";
  return "We couldn't complete that action. Check your connection and try again. If it keeps happening, reload the page.";
}

function ProfileEditor({ client }: { client: AuthyClient }) {
  const snapshot = useSyncExternalStore(client.subscribe, client.getSnapshot);
  const server = snapshot.profile;
  const [firstName, setFirstName] = useState(server?.first_name ?? "");
  const [lastName, setLastName] = useState(server?.last_name ?? "");
  const [lastSyncedRevision, setLastSyncedRevision] = useState<string | null>(
    server ? server.revision : null,
  );

  // Adopt the authoritative view when it first loads or when a new revision
  // arrives. Saving retains the form while awaiting authoritative replication.
  useEffect(() => {
    if (!server) return;
    if (lastSyncedRevision === null || server.revision !== lastSyncedRevision) {
      setFirstName(server.first_name);
      setLastName(server.last_name);
      setLastSyncedRevision(server.revision);
    }
  }, [server, lastSyncedRevision]);

  const save = (event: FormEvent) => {
    event.preventDefault();
    client.saveProfile(firstName, lastName);
  };

  return (
    <section>
      <h2>Your profile</h2>
      {server ? (
        <form onSubmit={save}>
          <label htmlFor="profile-name">First name</label>
          <input
            id="profile-name"
            name="first_name"
            autoComplete="given-name"
            value={firstName}
            maxLength={100}
            required
            onChange={(e) => setFirstName(e.target.value)}
          />
          <label htmlFor="profile-last-name">Last name</label>
          <input
            id="profile-last-name"
            name="last_name"
            autoComplete="family-name"
            value={lastName}
            maxLength={100}
            onChange={(e) => setLastName(e.target.value)}
          />
          <p className="muted" role="status">
            {snapshot.saving ? "Saving revision " : "Saved revision "}{server.revision}
          </p>
          <div className="actions">
            <button type="submit" disabled={snapshot.saving || snapshot.connection !== "connected"}>
              Save profile
            </button>
            <button
              type="button"
              className="secondary"
              onClick={() => {
                setFirstName(server.first_name);
                setLastName(server.last_name);
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
        {client.passkeysSupported() && <button className="secondary" disabled={busy} onClick={() => void act(() => client.passkey(true))}>Add a passkey</button>}
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
