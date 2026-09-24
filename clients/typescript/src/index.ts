export interface Options {
  baseUrl: string;
  build: string;
  wasm?: BufferSource | WebAssembly.Module;
}

export interface Client {
  readonly health: { up(): Promise<{ status: "OK" }> };
  close(): Promise<void>;
}

export interface ObservedClient<Snapshot> {
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

/** Each application keeps its own initialization cache for its generated module. */
export function initializeBindings(
  initialize: (input?: {
    module_or_path: NonNullable<Options["wasm"]>;
  }) => Promise<unknown>,
) {
  let initialization: Promise<unknown> | undefined;
  return async (wasm: Options["wasm"]) => {
    initialization ??= initialize(
      wasm === undefined ? undefined : { module_or_path: wasm },
    ).catch((error: unknown) => {
      initialization = undefined;
      throw error;
    });
    await initialization;
  };
}

/** Language facade only. Rust's browser runtime owns HTTP, deadlines and cancellation. */
export function queryClient(core: {
  healthUp(): Promise<string>;
  close(): void;
  free(): void;
}): Client {
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

/**
 * Owns subscriptions and binding lifetime. start returns before delivering changes;
 * decodeSnapshot returns deeply immutable values, cached until the next change.
 */
export function observeClient<Snapshot>(
  start: (changed: (wire: string) => void) => {
    snapshot(): string;
    close(): Promise<void>;
    free(): void;
  },
  decodeSnapshot: (wire: string) => Snapshot,
): ObservedClient<Snapshot> {
  const subscribers = new Set<() => void>();
  let closing: Promise<void> | undefined;
  const core = start((wire: string) => {
    if (closing) return;
    snapshot = decodeSnapshot(wire);
    for (const changed of subscribers) {
      try {
        changed();
      } catch (error) {
        console.error("Snapshot subscriber failed", error);
      }
    }
  });
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
