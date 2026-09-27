import assert from "node:assert/strict";
import type { Snapshot } from "../../client";

export interface IdentityClient {
  command(key: string, payload?: unknown): Promise<unknown>;
  snapshot(): Promise<Snapshot>;
  close(): Promise<void>;
  /** Fixture ownership only, outside the application-facing assertions. */
  dispose(): Promise<void>;
}

export async function observed(client: IdentityClient, predicate: (snapshot: Snapshot) => boolean): Promise<Snapshot> {
  const deadline = Date.now() + 10_000;
  let snapshot: Snapshot;
  do {
    snapshot = await client.snapshot();
    if (predicate(snapshot)) return snapshot;
    await new Promise(done => setTimeout(done, 20));
  } while (Date.now() < deadline);
  throw new Error(`Identity observation timed out: ${JSON.stringify(snapshot)}`);
}

/** Adapter smoke: detailed password/session policy lives in memory.rs. */
export async function passwordSessions(create: () => Promise<IdentityClient>) {
  const clients: IdentityClient[] = [];
  const open = async () => { const client = await create(); clients.push(client); await observed(client, s => s.phase === "anonymous"); return client; };
  try {
    const first = await open();
    const second = await open();
    await first.command("account.create", { email: " Person@EXAMPLE.test ", password: "password sessions" });
    const signedIn = await observed(first, s => s.connection === "connected" && s.sessions.length === 1 && s.credentials.length === 1);
    assert.equal(signedIn.phase, "identified");
    await second.command("identity.password.acquire", { kind: "user", email: "person@example.test", password: "password sessions" });
    const other = await observed(second, s => s.connection === "connected" && s.sessions.length === 2);
    assert.equal(other.identityId, signedIn.identityId);
    await first.command("refresh");
    await observed(first, s => s.sessions.length === 2);
    await second.command("identity.release", { scope: "others" });
    await observed(first, s => s.phase === "anonymous" && s.identityId === null && s.sessions.length === 0 && s.credentials.length === 0);
    await observed(second, s => s.connection === "connected" && s.sessions.length === 1);
    await first.close();
    assert.equal((await first.snapshot()).phase, "closed");
    assert.equal((await first.snapshot()).pending, false);
    await assert.rejects(first.command("refresh"));
  } finally { await Promise.all(clients.map(client => client.dispose())); }
}
