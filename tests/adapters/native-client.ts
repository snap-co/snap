import type { HealthyClient } from "../sdk/healthy.contract";
import { deadline } from "./server";

export async function nativeClient(baseUrl: string): Promise<HealthyClient> {
  const child = Bun.spawn(
    [
      new URL("../../target/debug/examples/client-bridge", import.meta.url)
        .pathname,
    ],
    {
      env: { ...process.env, SNAP_BASE_URL: baseUrl },
      stdin: "pipe",
      stdout: "pipe",
      stderr: "inherit",
    },
  );
  const reader = child.stdout.pipeThrough(new TextDecoderStream()).getReader();
  let buffered = "";
  return {
    health: {
      async up() {
        child.stdin.write("health.up\n");
        await child.stdin.flush();
        while (!buffered.includes("\n")) {
          const chunk = await reader.read();
          if (chunk.done)
            throw new Error("Native client exited without a result");
          buffered += chunk.value;
        }
        const newline = buffered.indexOf("\n");
        const result = JSON.parse(buffered.slice(0, newline));
        buffered = buffered.slice(newline + 1);
        if (result.error) throw new Error(JSON.stringify(result.error));
        return result.ok;
      },
    },
    async close() {
      child.stdin.end();
      try {
        await deadline(child.exited, 7_000);
      } catch {
        child.kill("SIGKILL");
        await child.exited;
      }
      await reader.cancel();
    },
  };
}
