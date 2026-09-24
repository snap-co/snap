import initialize, { Authy } from "./.snap/bindings/authy_wasm.js";
import { initializeBindings, type Options } from "../../clients/typescript/src";

export interface Snapshot {
  readonly phase: "loading" | "anonymous" | "identified" | "error" | "closed";
  readonly identityId: string | null;
  readonly connection: "disconnected" | "connecting" | "connected";
  readonly pending: boolean;
  readonly error: { _tag: string; message?: string; failure?: { message: string } } | null;
  readonly credentials: readonly { credentialId: string; label: string; method: string; createdAt: string; removable: boolean }[];
  readonly sessions: readonly { sessionId: string; createdAt: string; expiresAt: string; current: boolean }[];
}
export type Release = { scope: "current" | "others" | "all" } | { scope: "session"; sessionId: string };
const ready = initializeBindings(initialize);

export async function startAuthy(options: Options) {
  await ready(options.wasm);
  const listeners = new Set<() => void>();
  let closed = false;
  let snapshot: Snapshot;
  const decode = (wire: string): Snapshot => {
    const value: Snapshot = JSON.parse(wire);
    value.credentials.forEach(Object.freeze); Object.freeze(value.credentials);
    value.sessions.forEach(Object.freeze); Object.freeze(value.sessions);
    if (value.error) { if (value.error.failure) Object.freeze(value.error.failure); Object.freeze(value.error); }
    return Object.freeze(value);
  };
  const client = new Authy(options.baseUrl, options.build, (wire: string) => {
    if (closed) return;
    snapshot = decode(wire);
    for (const listener of listeners) listener();
  });
  snapshot = decode(client.snapshot());
  const command = async (key: string, payload?: unknown) => {
    if (closed) throw new Error("Client is closed");
    try { return JSON.parse(await client.command(key, payload === undefined ? undefined : JSON.stringify(payload))); }
    catch (wire) {
      if (typeof wire !== "string") throw wire;
      const error = JSON.parse(wire);
      throw Object.assign(new Error(error.failure?.message ?? error.message ?? "Identity operation failed"), error);
    }
  };
  return {
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) { if (!closed) listeners.add(listener); return () => { listeners.delete(listener); }; },
    createAccount: (email: string, password: string) => command("account.create", { email, password }),
    signIn: (email: string, password: string) => command("identity.password.acquire", { kind: "user", email, password }),
    release: (scope: Release) => command("identity.release", scope),
    refresh: () => command("refresh"),
    async close() { if (closed) return; closed = true; listeners.clear(); await client.close(); client.free(); },
  };
}
export type AuthyClient = Awaited<ReturnType<typeof startAuthy>>;
