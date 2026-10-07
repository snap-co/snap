/** React adapters for generated Rust/Wasm SDK exports. Wire validation and
 * authentication behavior belong to Rust; this layer only binds UI values. */
import type { Connected } from "../browser/transport";
export type Principal = { identity: string; authenticated_at: number };
export interface IdentityBinding {
  identity_fetch(): Promise<string>;
  identity_enroll(email: string, password: string): Promise<string>;
  identity_acquire(email: string, password: string): Promise<string>;
  identity_release(scope: string): Promise<void>;
  identity_sessions(invoke: Connected): Promise<string>;
  identity_credentials(invoke: Connected): Promise<string>;
}
// Wasm may reject with a string. React callers receive ordinary Error objects;
// messages are still supplied by the Rust SDK.
async function invoke<T>(result: Promise<T>): Promise<T> {
  try { return await result; }
  catch (error) { throw error instanceof Error ? error : new Error(String(error)); }
}
export function bindIdentity<M extends IdentityBinding, A>(load: () => Promise<M>, account: (module: M, principal: Principal) => Promise<A>, connected: () => Connected) {
  return {
    async fetch(): Promise<A | null> {
      const module = await load();
      const principal = JSON.parse(await invoke(module.identity_fetch())) as Principal | null;
      return principal ? invoke(account(module, principal)) : null;
    },
    async acquire(email: string, password: string, enroll: boolean): Promise<A> {
      const module = await load();
      const principal = JSON.parse(await invoke(enroll ? module.identity_enroll(email, password) : module.identity_acquire(email, password))) as Principal;
      return invoke(account(module, principal));
    },
    async release(scope: string): Promise<void> { await invoke((await load()).identity_release(scope)); },
    async sessions<T>(): Promise<T> { const call = connected(); return JSON.parse(await invoke((await load()).identity_sessions(call))) as T; },
    async credentials<T>(): Promise<T> { const call = connected(); return JSON.parse(await invoke((await load()).identity_credentials(call))) as T; },
  };
}
export function bindIdentityProjection<A>(fetch: () => Promise<string>) {
  return { async fetch(): Promise<A | null> { return JSON.parse(await invoke(fetch())) as A | null; } };
}
