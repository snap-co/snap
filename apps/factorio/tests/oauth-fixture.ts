// Only the disposable journey controller imports this. Mutate stopped fixture
// databases to reach expiry/revocation boundaries without changing host clocks.
import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";

export async function cliAuthorityFixture(
  request: Request,
  directory: string,
  authy: { database: string; restart: (beforeStart?: () => Promise<void>) => Promise<void> },
  stop: () => Promise<void>,
  start: () => Promise<void>,
): Promise<Response | undefined> {
  if (new URL(request.url).pathname !== "/cli-authority") return;
  const { owner, action } = await request.json() as { owner: string; action: string };
  if (!["state", "expire-access", "expire-login", "revoke-grant"].includes(action)) return new Response("invalid action", { status: 400 });
  const file = `${directory}/factorio.sqlite`;
  // The native store holds exclusive SQLite ownership while running. Inspect
  // snapshots only after shutdown, then restore the fixture for the next command.
  await stop();
  function session(db: Database) {
    const rows = db.query('SELECT id, data FROM "oidc_rp.sessions"').all() as { id: string; data: string }[];
    const found = rows.find(row => JSON.parse(row.data).owner === owner);
    if (!found) throw new Error("Missing fixture OAuth session");
    return { id: found.id, data: JSON.parse(found.data) };
  }
  if (action !== "state") {
    const db = new Database(file);
    try {
      const saved = session(db);
      if (action === "expire-login") {
        db.query('UPDATE "factorio.cli" SET expires = 0 WHERE session = ?').run(saved.id);
      }
      saved.data.tokens.access_expires = Math.floor(Date.now() / 1000) - 1;
      db.query('UPDATE "oidc_rp.sessions" SET data = ? WHERE id = ?').run(JSON.stringify(saved.data), saved.id);
      if (action === "revoke-grant") {
        await authy.restart(async () => {
          const issuer = new Database(authy.database);
          try {
            const digest = createHash("sha256").update(saved.data.tokens.refresh).digest();
            const result = issuer.query('UPDATE "oidc.grants" SET active = 0 WHERE id = (SELECT "grant" FROM "oidc.tokens" WHERE id = ?)').run(digest);
            if (result.changes !== 1) throw new Error("Missing fixture refresh grant");
          } finally { issuer.close(); }
        });
      }
    } finally { db.close(); }
  }
  const db = new Database(file, { readonly: true });
  let result;
  try {
    // Failed refresh deletes its backing session. No upstream tokens are exposed.
    const rows = db.query('SELECT data FROM "oidc_rp.sessions"').all() as { data: string }[];
    const saved = rows.map(row => JSON.parse(row.data)).find(value => value.owner === owner);
    result = saved ? {
      version: saved.version,
      access_expires: saved.tokens.access_expires,
      session_expires: saved.expires,
      cli_expires: (db.query('SELECT expires FROM "factorio.cli" WHERE session = ?').get(saved.id) as { expires: number }).expires,
    } : { revoked: true };
  } finally { db.close(); }
  await start();
  return Response.json(result);
}
