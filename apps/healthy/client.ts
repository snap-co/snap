import initialize, {
  Client as WasmClient,
  Healthy as WasmHealthy,
} from "./.snap/bindings/healthy_wasm.js";
import {
  initializeBindings,
  observeClient,
  queryClient,
  type Client,
  type ObservedClient,
  type Options,
} from "../../clients/typescript/src";

export { ClientError, type Client, type Options } from "../../clients/typescript/src";

export interface Snapshot {
  readonly status: "loading" | "ok" | "error";
  readonly samples: readonly { readonly ok: boolean; readonly at: number }[];
}

export type HealthyClient = ObservedClient<Snapshot>;

const ready = initializeBindings(initialize);

export async function createClient(options: Options): Promise<Client> {
  await ready(options.wasm);
  return queryClient(new WasmClient(options.baseUrl, options.build));
}

export async function startHealthy(options: Options): Promise<HealthyClient> {
  await ready(options.wasm);
  return observeClient(
    (changed) => new WasmHealthy(options.baseUrl, options.build, changed),
    decodeSnapshot,
  );
}

function decodeSnapshot(wire: string): Snapshot {
  const value: Snapshot = JSON.parse(wire);
  value.samples.forEach(Object.freeze);
  Object.freeze(value.samples);
  return Object.freeze(value);
}
