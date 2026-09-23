import initialize, {
  Client as WasmClient,
  Healthy as WasmHealthy,
} from "../wasm/snap_client_wasm.js";

export interface Options {
  baseUrl: string;
  build: string;
  wasm?: BufferSource | WebAssembly.Module;
}

export interface Client {
  readonly health: { up(): Promise<{ status: "OK" }> };
  close(): Promise<void>;
}

export interface Snapshot {
  readonly status: "loading" | "ok" | "error";
  readonly samples: readonly { readonly ok: boolean; readonly at: number }[];
}

export interface HealthyClient {
  getSnapshot(): Snapshot;
  subscribe(changed: () => void): () => void;
  close(): Promise<void>;
}

export class ClientError extends Error {
  constructor(
    readonly _tag: string,
    message: string,
  ) {
    super(message);
    this.name = _tag;
  }
}

let initialization: Promise<unknown> | undefined;
async function ready(wasm: Options["wasm"]) {
  initialization ??= initialize(
    wasm === undefined ? undefined : { module_or_path: wasm },
  ).catch((error: unknown) => {
    initialization = undefined;
    throw error;
  });
  await initialization;
}

/** Language facade only. Rust's browser runtime owns HTTP, deadlines and cancellation. */
export async function createClient(options: Options): Promise<Client> {
  await ready(options.wasm);
  const core = new WasmClient(options.baseUrl, options.build);
  const pending = new Set<Promise<unknown>>();
  let closing: Promise<void> | undefined;
  return {
    health: {
      up() {
        if (closing)
          return Promise.reject(
            new ClientError("UnavailableError", "Client is closed"),
          );
        const work = core.healthUp().then(
          (wire) => JSON.parse(wire) as { status: "OK" },
          (cause: unknown) => {
            throw clientError(cause);
          },
        );
        pending.add(work);
        void work.then(
          () => pending.delete(work),
          () => pending.delete(work),
        );
        return work;
      },
    },
    close() {
      closing ??= (async () => {
        core.close();
        await Promise.allSettled([...pending]);
        core.free();
      })();
      return closing;
    },
  };
}

/** Healthy composition is selected in Rust; this adapts observations to JS subscribers. */
export async function startHealthy(options: Options): Promise<HealthyClient> {
  await ready(options.wasm);
  const subscribers = new Set<() => void>();
  let closing: Promise<void> | undefined;
  const core = new WasmHealthy(
    options.baseUrl,
    options.build,
    (wire: string) => {
      if (closing) return;
      snapshot = decodeSnapshot(wire);
      for (const changed of subscribers) {
        try {
          changed();
        } catch (error) {
          console.error("Snapshot subscriber failed", error);
        }
      }
    },
  );
  let snapshot = decodeSnapshot(core.snapshot());
  return {
    getSnapshot: () => snapshot,
    subscribe(changed) {
      if (closing)
        throw new ClientError("UnavailableError", "Client is closed");
      subscribers.add(changed);
      return () => {
        subscribers.delete(changed);
      };
    },
    close() {
      closing ??= (async () => {
        subscribers.clear();
        await core.close();
        core.free();
      })();
      return closing;
    },
  };
}

function decodeSnapshot(wire: string): Snapshot {
  const value: Snapshot = JSON.parse(wire);
  value.samples.forEach(Object.freeze);
  Object.freeze(value.samples);
  return Object.freeze(value);
}

function clientError(cause: unknown): ClientError {
  if (cause instanceof ClientError) return cause;
  if (typeof cause === "string") {
    try {
      const error = JSON.parse(cause);
      if (typeof error._tag === "string" && typeof error.message === "string")
        return new ClientError(error._tag, error.message);
    } catch {
      /* Preserve unexpected binding failures as typed SDK errors. */
    }
    return new ClientError("ContractViolationError", cause);
  }
  return new ClientError(
    "UnavailableError",
    cause instanceof Error ? cause.message : String(cause),
  );
}
