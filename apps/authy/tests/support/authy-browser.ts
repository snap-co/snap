import type { Browser, Page } from "@playwright/test";
import { resolve } from "node:path";
import type { IdentityClient } from "../sdk/identity.contract";

export async function browserIdentity(browser: Browser, baseUrl: string, options: { module?: string; build?: string } = {}): Promise<IdentityClient & { page: Page }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  try {
    await page.goto(`${baseUrl}/__snap/build`);
    await page.evaluate(async ({ module, selectedBuild }) => {
      const { startAuthy } = await import(module);
      const { build } = await (await fetch("/__snap/build")).json();
      (globalThis as any).identitySDK = await startAuthy({ baseUrl: location.origin, build: selectedBuild ?? build, wasm: await (await fetch("/snap_client_wasm_bg.wasm")).arrayBuffer() });
    }, { module: options.module ?? `/@fs/${resolve(import.meta.dirname, "../../client.ts")}`, selectedBuild: options.build });
    let closed = false;
    const client: IdentityClient & { page: Page } = {
      page,
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
      async close() { if (closed) return; closed = true; await page.evaluate(() => (globalThis as any).identitySDK?.close()); },
      async dispose() { try { await client.close(); } finally { await context.close(); } },
    };
    return client;
  } catch (error) { await context.close(); throw error; }
}
