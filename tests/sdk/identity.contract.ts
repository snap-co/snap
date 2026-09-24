import assert from "node:assert/strict";
import type { Snapshot } from "../../apps/authy/client";

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

/** Same application-facing journey through native and browser Rust clients. */
export async function passwordSessions(create: () => Promise<IdentityClient>) {
  const clients: IdentityClient[] = [];
  const open = async () => { const client = await create(); clients.push(client); await observed(client, s => s.phase === "anonymous"); return client; };
  try {
    const first = await open();
    const second = await open();
    const duplicate = await open();
    await first.command("account.create", { email: " Person@EXAMPLE.test ", password: "password sessions" });
    const signedIn = await observed(first, s => s.connection === "connected" && s.sessions.length === 1 && s.credentials.length === 1);
    assert.equal(signedIn.phase, "identified");
    assert.equal(signedIn.credentials[0].label, "person@example.test");
    assert.equal(signedIn.credentials[0].removable, false);
    assert.equal(signedIn.sessions[0].current, true);
    assert.match(signedIn.identityId!, /^[0-9a-f-]{36}$/);
    await assert.rejects(second.command("identity.password.acquire", { kind: "user", email: "person@example.test", password: "incorrect" }), e => (e as any).failure?._tag === "InvalidCredentialError");
    assert.equal((await second.snapshot()).phase, "anonymous");
    await assert.rejects(duplicate.command("account.create", { email: "PERSON@example.test", password: "another password" }), e => (e as any).failure?._tag === "EnrollFailedError");
    await second.command("identity.password.acquire", { kind: "user", email: "person@example.test", password: "password sessions" });
    const other = await observed(second, s => s.connection === "connected" && s.sessions.length === 2);
    assert.equal(other.identityId, signedIn.identityId);
    assert.equal(other.sessions.filter(s => s.current).length, 1);
    await first.command("refresh");
    await observed(first, s => s.sessions.length === 2);
    await second.command("identity.release", { scope: "others" });
    await observed(first, s => s.phase === "anonymous" && s.identityId === null && s.sessions.length === 0 && s.credentials.length === 0);
    await observed(second, s => s.connection === "connected" && s.sessions.length === 1);
    await first.command("identity.password.acquire", { kind: "user", email: "person@example.test", password: "password sessions" });
    await observed(first, s => s.connection === "connected" && s.identityId === signedIn.identityId);
    await first.command("identity.release", { scope: "all" });
    await observed(first, s => s.phase === "anonymous");
    await observed(second, s => s.phase === "anonymous");
    await first.close();
    assert.equal((await first.snapshot()).phase, "closed");
    assert.equal((await first.snapshot()).pending, false);
    await assert.rejects(first.command("refresh"));
  } finally { await Promise.all(clients.map(client => client.dispose())); }
}
