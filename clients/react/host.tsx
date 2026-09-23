// Reusable platform host. The build selects the application definition.
import { createRoot, type Root } from "react-dom/client";
import type { ComponentType } from "react";
import type { Options } from "../typescript/src";

export interface Application<Client extends { close(): Promise<void> }> {
  start(options: Options): Promise<Client>;
  View: ComponentType<{ client: Client }>;
}

export async function run<Client extends { close(): Promise<void> }>(
  application: Application<Client>,
): Promise<() => Promise<void>> {
  const rootElement = document.getElementById("root");
  if (!rootElement) throw new Error("Missing root element");
  const lifetime = new AbortController();
  let client: Client | undefined;
  let root: Root | undefined;
  let closing: Promise<void> | undefined;
  const close = () =>
    (closing ??= (async () => {
      lifetime.abort();
      window.removeEventListener("pagehide", onHide);
      root?.unmount();
      await client?.close();
    })());
  const onHide = () => {
    void close();
  };
  window.addEventListener("pagehide", onHide, { once: true });
  window.addEventListener("pageshow", (event) => {
    if (event.persisted) location.reload();
  });
  try {
    const response = await fetch("/__snap/build", {
      signal: AbortSignal.any([lifetime.signal, AbortSignal.timeout(5_000)]),
      cache: "no-store",
    });
    if (!response.ok) throw new Error("Build discovery failed");
    const build = await response.json();
    if (build.contract !== 1 || typeof build.build !== "string")
      throw new Error("Unsupported Build document");
    const wasmResponse = await fetch("/snap_client_wasm_bg.wasm", {
      signal: AbortSignal.any([lifetime.signal, AbortSignal.timeout(15_000)]),
    });
    if (!wasmResponse.ok) throw new Error("WASM loading failed");
    client = await application.start({
      baseUrl: location.origin,
      build: build.build,
      wasm: await wasmResponse.arrayBuffer(),
    });
    // Startup can complete after page exit. Dispose that late client before mounting.
    if (lifetime.signal.aborted) {
      await client.close();
      return close;
    }
    root = createRoot(rootElement);
    const View = application.View;
    root.render(<View client={client} />);
    return close;
  } catch (error) {
    await close();
    throw error;
  }
}
