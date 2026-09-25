import { test, expect } from "bun:test";
import { relyingParty } from "../adapters/oidc-peer";
import { BrowserSession } from "../adapters/chatty";

test("Chatty RP rejects wrong claims, signatures, subjects and browser correlation", async () => {
  const peer = await relyingParty();
  try {
    const callback = async (browser: BrowserSession) => { const start = await browser.fetch("/auth/login"); expect(start.status).toBe(303); const code = await browser.fetch(start.headers.get("location")!); return code.headers.get("location")!; };
    for (const claims of [{ iss: "https://other-issuer.example" }, { aud: "other-client" }, { nonce: "wrong-nonce" }, { exp: 1 }, { iat: 9999999999 }, { sub: "" }, { azp: "another-client" }, { at_hash: "wrong-hash" }]) {
      peer.claims(claims);
      const browser = new BrowserSession(peer.baseUrl, peer.origin); const url = await callback(browser);
      expect((await browser.fetch(url)).status).toBe(401);
      expect((await (await browser.fetch("/api/session")).json()).identified).toBe(false);
    }
    peer.claims({}); peer.invalidSignature(true);
    let browser = new BrowserSession(peer.baseUrl, peer.origin); expect((await browser.fetch(await callback(browser))).status).toBe(400);
    peer.invalidSignature(false); peer.wrongUserInfo(true);
    browser = new BrowserSession(peer.baseUrl, peer.origin); expect((await browser.fetch(await callback(browser))).status).toBe(401);
    peer.wrongUserInfo(false);
    browser = new BrowserSession(peer.baseUrl, peer.origin); const url = await callback(browser);
    const other = new BrowserSession(peer.baseUrl, peer.origin); expect((await other.fetch(url)).status).toBe(400);
    const changed = new URL(url); changed.searchParams.set("iss", "https://attacker.example"); expect((await browser.fetch(changed.href)).status).toBe(400);
    browser = new BrowserSession(peer.baseUrl, peer.origin); const fresh = await callback(browser); expect((await browser.fetch(fresh)).status).toBe(303); expect((await browser.fetch(fresh)).status).toBe(400);
  } finally { await peer.close(); }
}, 60_000);

test("Chatty serializes refresh, preserves owner and fails closed on rejected refresh", async () => {
  const peer = await relyingParty();
  try {
    const login = async () => { const browser = new BrowserSession(peer.baseUrl, peer.origin); const start = await browser.fetch("/auth/login"); const authorization = await browser.fetch(start.headers.get("location")!); expect((await browser.fetch(authorization.headers.get("location")!)).status).toBe(303); return browser; };
    peer.ttl(1); const browser = await login();
    const responses = await Promise.all(Array.from({ length: 8 }, () => browser.fetch("/api/session")));
    for (const r of responses) { expect(r.status).toBe(200); expect((await r.json()).account.id).toBe("person"); }
    expect(peer.refreshes()).toBe(1);
    const rejected = await login(); peer.failRefresh();
    expect((await rejected.fetch("/api/session")).status).toBe(401);
    expect((await (await rejected.fetch("/api/session")).json()).identified).toBe(false);
    expect((await (await browser.fetch("/api/session")).json()).identified).toBe(true);
  } finally { await peer.close(); }
}, 60_000);
