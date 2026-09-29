/** Cache initialization, not just import, so concurrent clients share one module.
 * A failed initialization can be retried from the startup error screen. */
export function wasmModule<M extends { default(options: { module_or_path: string }): Promise<unknown> }>(name: string) {
  let pending: Promise<M> | undefined;
  return () => pending ??= (async () => {
    const path = `/bindings/${name}_wasm.js`;
    const module = await import(/* @vite-ignore */ path) as M;
    await module.default({ module_or_path: `/bindings/${name}_wasm_bg.wasm` });
    return module;
  })().catch(error => { pending = undefined; throw error; });
}
