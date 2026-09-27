import { test, expect } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";

test("intake tools retain scoped authorization after shell environment loss", async () => {
  const directory = await mkdtemp("/tmp/opencode/intake-tool-");
  const requests: unknown[] = [];
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    if (request.headers.get("authorization") !== "Bearer fixture-scoped-token") return new Response("unauthorized", { status: 401 });
    requests.push(await request.json());
    return Response.json({ revision: requests.length });
  } });
  try {
    const config = join(directory, "tool.json");
    await writeFile(config, JSON.stringify({ origin: `http://127.0.0.1:${server.port}`, token: "fixture-scoped-token" }), { mode: 0o600 });
    for (const command of ["intake-read", "intake-save"]) {
      const child = Bun.spawn([process.execPath, join(import.meta.dir, "../cli.ts"), command, ...(command === "intake-save" ? ["-"] : []), "--intake-config", config], { env: {}, stdin: new Blob([JSON.stringify({ revision: 1, route: "grill", rationale: "Needs scope", tickets: [] })]), stdout: "pipe", stderr: "pipe" });
      const error = await new Response(child.stderr).text();
      expect(await child.exited, error).toBe(0);
    }
    expect(requests).toEqual([{ action: "read" }, { revision: 1, route: "grill", rationale: "Needs scope", tickets: [] }]);
  } finally { server.stop(true); await rm(directory, { recursive: true, force: true }); }
});
