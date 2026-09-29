// Authy browser client: Identity SDK + connected Transport + Rust Document sync.
//
// JS owns actual network IO, reconnect timers and forms. Rust (AuthyClient
// from /bindings/authy_wasm.js) owns profile mutation behavior, optimistic
// reconciliation and wire correlation over snap_document::client::Client +
// wire::Wire and authy::registry(). There is no JS duplicate of mutation or
// reconciliation logic.
//
// Identity.acquire/enroll set the HttpOnly cookie over Transport's HTTP carrier.
// Identity.fetch restores account knowledge before Transport.connect opens a
// WebSocket. The browser platform injects the client ID; application code does
// not own credential endpoints or bearer material.
// authy.sessions returns {sessions:[{id,expires,current}]} and
// authy.credentials returns
// {credentials:[{label,kind:'password',removable:false}]}. The HttpOnly
// same-origin cookie is the auth; no bearer is exposed. Socket /transport
// carries standard snap_transport Command/Response frames; the browser sends
// Connect {bearer:"", client_id} and the host fills the bearer from the
// upgrade cookie. Cross-tab revocation arrives as a Document Reset followed
// by socket close; the browser then calls Identity.fetch and becomes
// anonymous when revoked instead of reconnecting with empty authority.

import { Invocations } from "../../platforms/document/client";
import { Identity } from "../../platforms/identity/client";
import { Transport } from "../../platforms/transport/browser";

export interface Account {
  identity: string;
  email: string;
  profile: string;
  authenticated_at: number;
}

export interface ProfileView {
  name: string;
  bio: string;
  revision: string;
}

export type ConnectionState = "disconnected" | "connecting" | "connected";

export type LogoutScope = "current" | "others" | "all";

export interface SessionSummary {
  id: string;
  expires: number;
  current: boolean;
}

export interface CredentialSummary {
  label: string;
  kind: "password";
  removable: boolean;
}

export interface AuthySnapshot {
  readonly phase: "loading" | "anonymous" | "signed-in";
  readonly account: Account | null;
  readonly profile: ProfileView | null;
  /** Outstanding optimistic intents (enqueue - completion). */
  readonly pending: number;
  readonly connection: ConnectionState;
  /** True while a profile save is in flight (pending > 0). */
  readonly saving: boolean;
  readonly error: string | null;
  /** Client publication counter for reactive refresh. */
  readonly revision: string;
  /** Signed-in session list; null while signed out or not yet loaded. */
  readonly sessions: readonly SessionSummary[] | null;
  /** Credential labels; null while signed out or not yet loaded. */
  readonly credentials: readonly CredentialSummary[] | null;
}

// Structural shape of the generated wasm-bindgen module. The file itself is
// produced by the coordinator-owned build script
// (wasm-bindgen --out-dir ${AUTHY_BUILD_DIR or apps/authy/.snap/web}/bindings)
// and served under /bindings/authy_wasm.js + /bindings/authy_wasm_bg.wasm, so
// it is loaded with a runtime dynamic import and never committed.
interface WasmAuthyClient {
  actor(): string;
  profile_id(): string;
  snapshot(): string;
  connect_command(client_id: string): string;
  invoke(operation: string, input: string): string;
  attached(resumed: boolean): string;
  enqueue_edit(name: string, bio: string): string;
  receive(frame: string): string;
  free(): void;
}

interface WasmAuthyModule {
  default: (options?: { module_or_path: string }) => Promise<unknown>;
  AuthyClient: new (actor: string, profile: string) => WasmAuthyClient;
}

interface WasmResult {
  snapshot: {
    actor: string;
    profile_id: string;
    profile: {
      id: string;
      kind: string;
      version: string;
      revision: string;
      value: { name?: unknown; bio?: unknown };
    } | null;
    pending: number;
    awaiting_ack: string | null;
    reconciling: boolean;
    needs_recovery: boolean;
    revision: string;
  };
  send: string[];
  error: string | null;
}

function friendlyError(raw: string | null): string | null {
  if (!raw) return null;
  if (/edit rejected/i.test(raw) || /profile changed/i.test(raw)) {
    return "Edit rejected: the profile changed. Review the current values and retry your save.";
  }
  if (/NeedManifest/i.test(raw)) {
    return "Profile out of sync; reloading the current profile. Retry your save if it does not land.";
  }
  if (/NotFound.*profile not loaded/i.test(raw)) {
    return "Profile is still loading; wait for the connection, then retry your save.";
  }
  if (/InvalidBearer|Invalid Bearer/i.test(raw)) {
    return "Session expired; sign in again.";
  }
  return raw;
}

function profileOf(result: WasmResult): ProfileView | null {
  const snapshot = result.snapshot.profile;
  if (!snapshot || typeof snapshot.value !== "object" || !snapshot.value) return null;
  const name = (snapshot.value as { name?: unknown }).name;
  const bio = (snapshot.value as { bio?: unknown }).bio;
  if (typeof name !== "string" || typeof bio !== "string") return null;
  return { name, bio, revision: snapshot.revision };
}

function safeContinue(value: string | null): string | null {
  if (!value || !value.startsWith("/oauth/")) return null;
  // Local path only: no scheme, host, backslash or control characters.
  if (value.includes("\\") || value.includes("//") || /[\s<>"]/.test(value)) return null;
  if (!/^\/oauth\/[A-Za-z0-9._~:/?#[\]@!$&'()*+,;=%-]*$/.test(value)) return null;
  return value;
}

let wasmModule: WasmAuthyModule | null = null;
async function loadWasm(): Promise<WasmAuthyModule> {
  if (wasmModule) return wasmModule;
  const bindings = "/bindings/authy_wasm.js";
  const imported = (await import(/* @vite-ignore */ bindings)) as unknown as WasmAuthyModule;
  await imported.default({ module_or_path: "/bindings/authy_wasm_bg.wasm" });
  wasmModule = imported;
  return imported;
}

export interface AuthyClient {
  getSnapshot(): AuthySnapshot;
  subscribe(listener: () => void): () => void;
  signup(email: string, password: string): Promise<void>;
  login(email: string, password: string): Promise<void>;
  logout(scope?: LogoutScope): Promise<void>;
  /** Optimistic save; revision is read in Rust from the projected view. */
  saveProfile(name: string, bio: string): void;
  /** Fetch session/credential summaries on demand (also refreshed after auth actions). */
  refreshSessions(): Promise<void>;
  retryConnection(): void;
  close(): void;
}

export async function startAuthy(): Promise<AuthyClient> {
  const listeners = new Set<() => void>();
  const transport = new Transport();
  const identity = new Identity<Account>(transport);
  const readSession = () => identity.fetch();

  let snapshot: AuthySnapshot = {
    phase: "loading",
    account: null,
    profile: null,
    pending: 0,
    connection: "disconnected",
    saving: false,
    error: null,
    revision: "0",
    sessions: null,
    credentials: null,
  };
  let wasm: WasmAuthyClient | null = null;
  let socket: WebSocket | null = null;
  let closed = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let backoff = 250;
  let accountEpoch = 0;
  let authBusy = false;
  const calls = new Invocations((operation, input) => {
    if (!wasm || snapshot.connection !== "connected") throw new Error("Authy is still connecting");
    return wasm.invoke(operation, JSON.stringify(input));
  }, frame => {
    if (!socket || socket.readyState !== WebSocket.OPEN) throw new Error("Disconnected");
    socket.send(frame);
  });

  const emit = () => {
    for (const listener of listeners) listener();
  };
  const set = (next: Partial<AuthySnapshot>) => {
    snapshot = Object.freeze({ ...snapshot, ...next });
    emit();
  };

  const clearSocket = () => {
    calls.detached();
    if (socket) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      try {
        socket.close();
      } catch {
        // Closing a dead socket is best-effort.
      }
      socket = null;
    }
  };

  const applyWasmResult = (raw: string) => {
    const result = JSON.parse(raw) as WasmResult;
    const profile = profileOf(result);
    set({
      profile,
      pending: result.snapshot.pending,
      saving: result.snapshot.pending > 0,
      revision: result.snapshot.revision,
      error: friendlyError(result.error) ?? (result.snapshot.pending > 0 ? snapshot.error : null),
    });
    for (const command of result.send) sendFrame(command);
    // A revoked bearer surfaces as a Failed frame: re-fetch the session so
    // the tab becomes anonymous instead of reconnecting with no authority.
    if (result.error && /InvalidBearer|Invalid Bearer|StaleConnection/i.test(result.error)) {
      void revalidateSession();
      return;
    }
    // A newly reconciled profile clears a stale sync error.
    if (profile && result.error === null && snapshot.error !== null && result.snapshot.pending === 0) {
      set({ error: null });
    }
  };

  const sendFrame = (command: string) => {
    if (socket && socket.readyState === WebSocket.OPEN) {
      socket.send(command);
    }
  };

  const scheduleReconnect = () => {
    if (closed) return;
    if (reconnectTimer) return;
    set({ connection: "disconnected" });
    const delay = Math.min(backoff, 5000);
    backoff = Math.min(backoff * 2, 5000);
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (!closed && snapshot.account) connect();
    }, delay);
  };

  const teardownAccount = () => {
    calls.close("Account session ended");
    accountEpoch++;
    clearSocket();
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (wasm) {
      try {
        wasm.free();
      } catch {
        // Account teardown is best-effort.
      }
      wasm = null;
    }
    set({
      account: null,
      profile: null,
      pending: 0,
      saving: false,
      phase: "anonymous",
      connection: "disconnected",
      sessions: null,
      credentials: null,
    });
  };

  // Re-fetch the HTTP session: a revoked session (for example signed out
  // from another tab) reports account:null, and this tab becomes anonymous
  // instead of reconnecting forever with empty authority.
  const revalidateSession = async (): Promise<Account | null> => {
    const epoch = accountEpoch;
    let account: Account | null;
    try {
      account = await readSession();
    } catch {
      if (epoch === accountEpoch) scheduleReconnect();
      return snapshot.account;
    }
    if (closed || epoch !== accountEpoch) return snapshot.account;
    if (!account) {
      teardownAccount();
      return null;
    }
    if (account.identity !== snapshot.account?.identity) {
      accountEpoch++;
      set({ account, profile: null, pending: 0, saving: false, error: null, sessions: null, credentials: null });
      await resetDoc(account);
      set({ phase: "signed-in" });
      void fetchSummaries().catch(() => {});
    } else {
      set({ account, phase: "signed-in" });
    }
    return account;
  };

  // Session/credential summaries are fetched on demand and after auth
  // actions, never polled. Identity owns these business rules; the browser
  // only renders the summaries.
  const fetchSummaries = async (): Promise<void> => {
    if (closed || !snapshot.account || snapshot.connection !== "connected") return;
    const epoch = accountEpoch;
    const [sessions, credentials] = await Promise.all([
      calls.invoke<{ sessions: SessionSummary[] }>("authy.sessions", null),
      calls.invoke<{ credentials: CredentialSummary[] }>("authy.credentials", null),
    ]);
    if (closed || !snapshot.account || epoch !== accountEpoch) return;
    if (closed || !snapshot.account || epoch !== accountEpoch) return;
    set({
      sessions: Object.freeze(sessions.sessions.slice()),
      credentials: Object.freeze(credentials.credentials.slice()),
    });
  };

  function connect() {
    if (closed || !snapshot.account || !wasm) return;
    clearSocket();
    set({ connection: "connecting" });
    const binding = wasm;
    const next = transport.connect(clientId => binding.connect_command(clientId));
    socket = next;
    next.onopen = () => {
      if (socket !== next || closed) return;
      backoff = 250;
    };
    next.onmessage = (event) => {
      if (socket !== next || closed) return;
      const frame = typeof event.data === "string" ? event.data : String(event.data);
      try {
        if (calls.receive(frame)) return;
        const parsed = JSON.parse(frame) as { Attached?: { resumed: boolean } };
        if (parsed && typeof parsed.Attached === "object" && parsed.Attached) {
          set({ connection: "connected" });
          applyWasmResult(wasm!.attached(parsed.Attached.resumed === true));
          void fetchSummaries().catch(error => set({ error: String(error) }));
          return;
        }
      } catch {
        // Not an Attached frame; fall through to the Rust correlator.
      }
      try {
        set({ connection: "connected" });
        applyWasmResult(wasm!.receive(frame));
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        set({ error: message });
        // A dead bearer (revoked elsewhere) must not reconnect forever:
        // re-fetch the session and go anonymous when it is gone.
        if (/InvalidBearer|Invalid Bearer|StaleConnection/i.test(message)) {
          void revalidateSession();
        }
      }
    };
    next.onclose = () => {
      if (socket !== next || closed) return;
      socket = null;
      calls.detached();
      // The server closes revoked sockets after a Document Reset. Re-fetch
      // the session: a revoked tab becomes anonymous (profile cleared) while
      // a live session reconnects normally.
      void revalidateSession().then((account) => {
        if (account && !closed) scheduleReconnect();
      });
    };
    next.onerror = () => {
      // onclose follows and drives the reconnect timer.
    };
  }

  const resetDoc = async (account: Account | null) => {
    calls.close("Account changed");
    const epoch = accountEpoch;
    clearSocket();
    if (wasm) {
      try {
        wasm.free();
      } catch {
        // Dropping the previous account sync is best-effort.
      }
      wasm = null;
    }
    if (!account) return;
    const module = await loadWasm();
    if (closed) return;
    // Actor comes from the HTTP account identity; the profile id comes from
    // the account profile field. The document revision for each edit is read
    // in Rust from the projected view, never from a JS number.
    if (closed || epoch !== accountEpoch) return;
    wasm = new module.AuthyClient(account.identity, account.profile);
    const initial = JSON.parse(wasm.snapshot()) as WasmResult["snapshot"];
    set({
      profile: null,
      pending: initial.pending,
      saving: false,
      revision: initial.revision,
      error: null,
    });
    connect();
  };

  const refreshSession = async (): Promise<Account | null> => {
    const epoch = accountEpoch;
    const account = await readSession();
    if (closed || epoch !== accountEpoch) return snapshot.account;
    if ((account?.identity ?? null) !== (snapshot.account?.identity ?? null)) {
      accountEpoch++;
      set({ account, profile: null, pending: 0, saving: false, error: null, sessions: null, credentials: null });
      await resetDoc(account);
    } else {
      set({ account });
    }
    if (snapshot.phase === "loading") {
      set({ phase: account ? "signed-in" : "anonymous" });
    } else {
      set({ phase: account ? "signed-in" : "anonymous" });
    }
    if (account) void fetchSummaries().catch(() => {});
    return account;
  };

  const afterAuth = async (account: Account) => {
    set({ account, phase: "signed-in", error: null, sessions: null, credentials: null });
    await resetDoc(account);
    void fetchSummaries().catch(() => {});
    const next = safeContinue(new URLSearchParams(location.search).get("continue"));
    if (next) location.assign(next);
  };

  const acquire = async (enroll: boolean, email: string, password: string) => {
    if (authBusy) throw new Error("Another sign-in action is still pending.");
    authBusy = true;
    const epoch = ++accountEpoch;
    try {
    const value = await (enroll ? identity.enroll({ email, password }) : identity.acquire({ email, password }));
    if (closed || epoch !== accountEpoch) return;
    await afterAuth(value.account);
    } finally { authBusy = false; }
  };

  try {
    if (new URLSearchParams(location.search).get("reauth") === "1") {
      set({ phase: "anonymous" });
    } else {
      await refreshSession();
    }
  } catch (e) {
    if (!closed) set({ phase: "anonymous", error: e instanceof Error ? e.message : String(e) });
  }

  return {
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    async signup(email: string, password: string) {
      set({ error: null });
      await acquire(true, email, password);
    },
    async login(email: string, password: string) {
      set({ error: null });
      await acquire(false, email, password);
    },
    async logout(scope: LogoutScope = "current") {
      if (authBusy) throw new Error("Another sign-in action is still pending.");
      authBusy = true;
      const epoch = ++accountEpoch;
      try {
      set({ error: null });
      await calls.invoke("authy.logout", { scope });
      if (closed || epoch !== accountEpoch) return;
      if (scope === "others") {
        // The current session survives; refresh the account and summaries.
        try {
          await refreshSession();
        } catch (e) {
          set({ error: e instanceof Error ? e.message : String(e) });
        }
        return;
      }
      teardownAccount();
      } finally { authBusy = false; }
    },
    async refreshSessions() {
      await fetchSummaries();
    },
    saveProfile(name: string, bio: string) {
      if (!wasm || !snapshot.account) {
        set({ error: "Sign in before saving your profile." });
        return;
      }
      try {
        // Revision-safe: Rust reads the projected base revision for this
        // intent. A concurrent change rejects with an "edit rejected: profile
        // changed" error and the current view stays for retry.
        applyWasmResult(wasm.enqueue_edit(name, bio));
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },
    retryConnection() {
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      backoff = 250;
      if (snapshot.account) connect();
      else void refreshSession().catch(() => scheduleReconnect());
    },
    close() {
      calls.close();
      closed = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      clearSocket();
      if (wasm) {
        try {
          wasm.free();
        } catch {
          // Shutdown cleanup is best-effort.
        }
        wasm = null;
      }
      listeners.clear();
    },
  };
}

export type { WasmResult };
