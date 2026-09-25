import { createServer } from "node:http";
import { generateKeyPairSync, sign, randomBytes } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { freePort } from "./oidc";
import { startServer } from "./server";
const root = resolve(import.meta.dirname, "../..");

/** Independent issuer fixture lets the RP contract vary signed claims and refresh
 * outcomes without changing production clocks or reaching private session rows. */
export async function relyingParty() {
  const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const jwk = { ...publicKey.export({ format: "jwk" }), alg: "RS256", use: "sig", kid: "independent" };
  let origin = "";
  let claims: Record<string, unknown> = {};
  let invalidSignature = false;
  let ttl = 600;
  let refreshes = 0;
  let failRefresh = false;
  let wrongUserInfo = false;
  const grants = new Map<string, { nonce: string }>();
  const refresh = new Map<string, { nonce: string }>();
  const server = createServer(async (request, response) => {
    const url = new URL(request.url!, origin);
    const send = (status: number, value: unknown) => { response.writeHead(status, { "content-type": "application/json" }); response.end(JSON.stringify(value)); };
    if (url.pathname === "/.well-known/openid-configuration") return send(200, { issuer: origin, authorization_endpoint: `${origin}/authorize`, token_endpoint: `${origin}/token`, jwks_uri: `${origin}/jwks`, userinfo_endpoint: `${origin}/userinfo`, end_session_endpoint: `${origin}/logout` });
    if (url.pathname === "/jwks") return send(200, { keys: [jwk] });
    if (url.pathname === "/userinfo") return send(200, { sub: wrongUserInfo ? "other-person" : "person", name: "Fixture person", email: "fixture@example.test" });
    if (url.pathname === "/authorize") {
      const code = randomBytes(32).toString("base64url"); grants.set(code, { nonce: url.searchParams.get("nonce")! });
      const redirect = new URL(url.searchParams.get("redirect_uri")!); redirect.searchParams.set("code", code); redirect.searchParams.set("state", url.searchParams.get("state")!); redirect.searchParams.set("iss", origin);
      response.writeHead(303, { location: redirect.href }); response.end(); return;
    }
    if (url.pathname === "/token") {
      const chunks: Buffer[] = []; for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const form = new URLSearchParams(Buffer.concat(chunks).toString());
      const isRefresh = form.get("grant_type") === "refresh_token";
      const key = form.get(isRefresh ? "refresh_token" : "code")!;
      const source = isRefresh ? refresh : grants; const grant = source.get(key); source.delete(key);
      if (isRefresh) { refreshes++; await new Promise(done => setTimeout(done, 60)); }
      if (!grant || isRefresh && failRefresh) return send(400, { error: "invalid_grant" });
      const next = randomBytes(32).toString("base64url"); refresh.set(next, grant);
      const now = Math.floor(Date.now() / 1000);
      const payload = { iss: origin, sub: "person", aud: "chatty", iat: now, exp: now + 600, auth_time: 1000, nonce: grant.nonce, ...claims };
      const input = `${Buffer.from(JSON.stringify({ alg: "RS256", kid: "independent" })).toString("base64url")}.${Buffer.from(JSON.stringify(payload)).toString("base64url")}`;
      const signature = sign("RSA-SHA256", Buffer.from(input), privateKey); if (invalidSignature) signature[0] ^= 1;
      return send(200, { access_token: "fixture-access", refresh_token: next, token_type: "Bearer", expires_in: isRefresh ? 600 : ttl, id_token: `${input}.${signature.toString("base64url")}` });
    }
    send(404, {});
  });
  await new Promise<void>(done => server.listen(0, "127.0.0.1", done));
  const address = server.address(); if (!address || typeof address === "string") throw new Error("Missing issuer port"); origin = `http://127.0.0.1:${address.port}`;
  const directory = await mkdtemp("/tmp/opencode/chatty-rp-");
  const chattyAddress = `127.0.0.1:${await freePort()}`;
  const app = await startServer({ executable: resolve(root, "target/debug/chatty"), address: chattyAddress, env: { SNAP_ORIGIN: `http://${chattyAddress}`, SNAP_DATABASE: resolve(directory, "chatty.sqlite"), AUTHY_ORIGIN: origin, CHATTY_CLIENT_SECRET: "independent-issuer-fixture-32-characters", CHATTY_FILES: resolve(directory, "files") } });
  return { baseUrl: app.baseUrl, origin, claims: (value: Record<string, unknown>) => { claims = value; }, invalidSignature: (value: boolean) => { invalidSignature = value; }, ttl: (value: number) => { ttl = value; }, wrongUserInfo: (value: boolean) => { wrongUserInfo = value; }, failRefresh: () => { failRefresh = true; }, refreshes: () => refreshes, close: async () => { await app.close(); server.closeAllConnections(); await new Promise<void>(done => server.close(() => done())); await rm(directory, { recursive: true, force: true }); } };
}
