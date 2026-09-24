import { createClient } from "../apps/healthy/client";
import { healthyContract } from "../tests/sdk/healthy.contract";
import { referenceClient } from "../tests/adapters/reference-client";
import { nativeClient } from "../tests/adapters/native-client";
import { healthyProtocol } from "../tests/protocol/healthy.contract";
import { deadline, startServer } from "../tests/adapters/server";

const server = process.env.SNAP_BASE_URL ? undefined : await startServer();
try {
  const baseUrl = process.env.SNAP_BASE_URL ?? server!.baseUrl;
  const build = await healthyProtocol(baseUrl);
  console.log(
    "PASS: HTTP correlation, rejection, readiness, and Build discovery",
  );
  for (const implementation of [
    {
      name: "TypeScript reference",
      create: () => referenceClient(baseUrl, build),
    },
    { name: "native Rust", create: () => nativeClient(baseUrl) },
    {
      name: "Rust/WASM",
      create: async () =>
        createClient({
          baseUrl,
          build: build.build,
          wasm: await Bun.file(
            new URL(
              "../apps/healthy/.snap/bindings/healthy_wasm_bg.wasm",
              import.meta.url,
            ),
          ).arrayBuffer(),
        }),
    },
  ]) {
    const client = await implementation.create();
    try {
      await deadline(healthyContract(client), 7_000);
      console.log(
        `PASS: same Healthy SDK contract through ${implementation.name}`,
      );
    } finally {
      await client.close();
    }
  }
  const journey = Bun.spawn(
    [
      new URL("../target/debug/examples/healthy-journey", import.meta.url)
        .pathname,
    ],
    {
      env: { ...process.env, SNAP_BASE_URL: baseUrl },
      stdout: "inherit",
      stderr: "inherit",
    },
  );
  try {
    if ((await deadline(journey.exited, 12_000)) !== 0)
      throw new Error("Native journey failed");
  } finally {
    if (journey.exitCode === null) {
      journey.kill();
      await journey.exited;
    }
  }
} finally {
  await server?.close();
}
