import { afterAll, test, expect } from "bun:test";
import { chromium, type Browser } from "@playwright/test";
import assert from "node:assert/strict";
import { identityPeer } from "../support/identity-peer";
import { nativeIdentity } from "../support/authy-native";
import { browserIdentity } from "../support/authy-browser";
import { observed, type IdentityClient } from "./identity.contract";
import { deadline } from "../../../../tests/adapters/server";

let browser: Browser | undefined;
afterAll(async () => { await browser?.close(); });

const kinds = process.env.SNAP_TEST_PLATFORM === "native" ? ["native"] as const : ["browser"] as const;
for (const kind of kinds) {
  const open = async (baseUrl: string, build?: string) => {
    if (kind === "native") return nativeIdentity(baseUrl, { build });
    browser ??= await chromium.launch();
    return browserIdentity(browser, baseUrl, { build, module: "/apps/authy/client.ts" });
  };
  const upgrade = async (client: IdentityClient, trigger: () => void | Promise<unknown>) => {
    if ("page" in client) {
      const page = (client as Awaited<ReturnType<typeof browserIdentity>>).page;
      const navigated = page.waitForEvent("framenavigated", frame => frame === page.mainFrame());
      await trigger();
      await deadline(navigated, 5_000);
      expect(await page.evaluate(() => "identitySDK" in globalThis)).toBe(false);
    } else {
      await trigger();
      await observed(client, s => s.phase === "closed");
      await assert.rejects(client.command("refresh"));
    }
  };

  test(`${kind}: stale HTTP Build before socket attachment takes the upgrade path`, async () => {
    const peer = await identityPeer();
    const held = peer.hold("/identity/fetch");
    let client: IdentityClient | undefined;
    try {
      client = await open(peer.baseUrl, "outdated-build");
      await deadline(held.arrived, 5_000);
      await upgrade(client, held.release);
      expect(peer.count("/identity/fetch")).toBe(1);
    } finally { held.release(); try { await client?.dispose(); } finally { await peer.close(); } }
  }, 20_000);

  test(`${kind}: anonymous Submit Build mismatch upgrades without retrying the command`, async () => {
    const peer = await identityPeer();
    let client: IdentityClient | undefined;
    try {
      client = await open(peer.baseUrl);
      await observed(client, s => s.phase === "anonymous");
      peer.changeBuild();
      await upgrade(client, () => assert.rejects(client!.command("account.create", { email: "upgrade@example.test", password: "password sessions" })));
      expect(peer.count("/account/create")).toBe(1);
    } finally { try { await client?.dispose(); } finally { await peer.close(); } }
  }, 20_000);

  test(`${kind}: untrusted mutation outcomes refetch session authority without replay`, async () => {
    for (const body of ["{", JSON.stringify({ key: "transport.complete", target: "different-operation", payload: { ok: true } })]) {
      const peer = await identityPeer();
      let client: IdentityClient | undefined;
      try {
        client = await open(peer.baseUrl);
        await observed(client, s => s.phase === "anonymous");
        peer.corrupt(body);
        await assert.rejects(client.command("account.create", { email: "uncertain@example.test", password: "password sessions" }));
        const state = await observed(client, s => s.phase === "identified");
        expect(state.identityId).toMatch(/^[0-9a-f-]{36}$/);
        expect(state.pending).toBe(false);
        expect(peer.count("/account/create")).toBe(1);
        expect(peer.count("/identity/fetch")).toBe(2);
        await client.close();
        const closed = await client.snapshot();
        expect(closed.phase).toBe("closed");
        expect(closed.identityId).toBeNull();
        expect(closed.credentials).toEqual([]);
        expect(closed.sessions).toEqual([]);
      } finally { try { await client?.dispose(); } finally { await peer.close(); } }
    }
  }, 30_000);

  test(`${kind}: closing a pending command publishes the final closed snapshot`, async () => {
    const peer = await identityPeer();
    const held = peer.hold("/account/create");
    let client: IdentityClient | undefined;
    try {
      client = await open(peer.baseUrl);
      await observed(client, s => s.phase === "anonymous");
      const command = client.command("account.create", { email: "closing@example.test", password: "password sessions" });
      const rejected = assert.rejects(command);
      await deadline(held.arrived, 5_000);
      await observed(client, s => s.pending);
      await deadline(client.close(), 2_000);
      await deadline(rejected, 2_000);
      const state = await client.snapshot();
      expect(state.phase).toBe("closed");
      expect(state.pending).toBe(false);
      expect(state.identityId).toBeNull();
      await assert.rejects(client.command("refresh"));
    } finally { held.release(); try { await client?.dispose(); } finally { await peer.close(); } }
  }, 20_000);
}
