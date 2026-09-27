import { Database } from "bun:sqlite";
import { createHash, createHmac } from "node:crypto";

/** Pre-refactor Authy schema, kept independent of the new Store declarations. */
export async function legacyPassport(path: string) {
  const database=new Database(path,{create:true});
  const identity=crypto.randomUUID(), credential=crypto.randomUUID(), session=crypto.randomUUID();
  const token="legacy-session-token", key="legacy-signing-key-with-at-least-32-bytes";
  const hash=await Bun.password.hash("original password", "argon2id");
  try {
    database.exec(`CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE credentials (id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, kind TEXT NOT NULL, email TEXT NOT NULL, hash TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE(kind,email));
      CREATE TABLE sessions (id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, credential_id TEXT NOT NULL REFERENCES credentials(id), digest TEXT NOT NULL UNIQUE, created INTEGER NOT NULL, expires INTEGER NOT NULL);
      CREATE INDEX session_identity ON sessions(identity_id);`);
    database.query("INSERT INTO settings VALUES (?,?)").run("signing-key",key);
    database.query("INSERT INTO credentials VALUES (?,?,?,?,?,?)").run(credential,identity,"user","legacy@example.test",hash,Date.now());
    database.query("INSERT INTO sessions VALUES (?,?,?,?,?,?)").run(session,identity,credential,createHash("sha256").update(token).digest("base64url"),Date.now(),Date.now()+86400000);
  } finally {database.close();}
  return {identity, cookie:`authy_session=${token}.${createHmac("sha256",key).update(token).digest("base64url")}`};
}
