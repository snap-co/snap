import type { Browser } from "@playwright/test";
import { resolve } from "node:path";
import type { IdentityClient } from "../sdk/identity.contract";

export async function browserIdentity(browser: Browser, baseUrl: string): Promise<IdentityClient> {
  const context = await browser.newContext();
  const page = await context.newPage();
  try {
    await page.goto(`${baseUrl}/__snap/build`);
    await page.evaluate(async (module) => {
      const { startAuthy } = await import(module);
      const { build } = await (await fetch("/__snap/build")).json();
      (globalThis as any).identitySDK = await startAuthy({ baseUrl: location.origin, build, wasm: await (await fetch("/snap_client_wasm_bg.wasm")).arrayBuffer() });
    }, `/@fs/${resolve(import.meta.dirname, "../../apps/authy/client.ts")}`);
    let closed = false;
    return {
      async command(key, payload) {
        if (closed) throw new Error("Client is closed");
        const result = await page.evaluate(async ({ key, payload }) => {
          const client = (globalThis as any).identitySDK;
          try {
            let value;
            if (key === "account.create") value = await client.createAccount(payload.email, payload.password);
            else if (key === "identity.password.acquire") value = await client.signIn(payload.email, payload.password);
            else if (key === "identity.release") value = await client.release(payload);
            else value = await client.refresh();
            return { ok: true, value };
          } catch (error) { return { ok: false, error: JSON.parse(JSON.stringify(error)) }; }
        }, { key, payload: payload as any });
        if (!result.ok) throw result.error;
        return result.value;
      },
      snapshot: () => page.evaluate(() => (globalThis as any).identitySDK.getSnapshot()),
      async close() { if (closed) return; closed = true; try { await page.evaluate(() => (globalThis as any).identitySDK.close()); } finally { await context.close(); } },
    };
  } catch (error) { await context.close(); throw error; }
}
