// Construction adapter only. The behavioral assertions live in healthy.contract.ts.
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import type { HealthyClient } from "../sdk/healthy.contract";

export async function referenceClient(
  baseUrl: string,
  build: { contract: 1; application: string; build: string },
): Promise<HealthyClient> {
  const reference =
    process.env.SNAP_REFERENCE ?? join(homedir(), "code/bod/snap");
  const resolve = createRequire(join(reference, "package.json")).resolve;
  const [
    { Effect, Layer, ManagedRuntime },
    { Health, Uuid },
    { Doctor, Transport },
    { BrowserTransport },
  ] = await Promise.all([
    import(resolve("effect")),
    import(resolve("@snap/core")),
    import(resolve("@snap/engine")),
    import(resolve("@snap/browser/transport")),
  ]);
  const runtime = ManagedRuntime.make(
    Doctor.clientLayer().pipe(
      Layer.provide(Transport.Client().layer),
      Layer.provide(BrowserTransport.Client.layer({ baseUrl, build })),
      Layer.provide(Uuid.layer(Uuid.default())),
    ),
  );
  return {
    health: {
      up: () =>
        runtime.runPromise(
          Effect.gen(function* () {
            const client = yield* Health.Client;
            return yield* client.up();
          }),
        ),
    },
    close: () => runtime.dispose(),
  };
}
