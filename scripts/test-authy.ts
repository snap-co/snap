import { host } from "../apps/authy/tests/support/upgraded-host";
const callback = Bun.serve({ hostname: "127.0.0.1", port: 0,
  fetch: () => new Response("OAuth callback fixture", { headers: { "content-type": "text/html" } }),
});
const relyingPartyOrigin = `http://127.0.0.1:${callback.port}`;
let server: Awaited<ReturnType<typeof host>> | undefined;
try {
  server = await host(relyingPartyOrigin);
  const child = Bun.spawn(["bunx", "playwright", "test", "--config", "apps/authy/tests/upgraded-playwright.config.ts"], {
    env: { ...process.env, AUTHY_TEST_URL: server.base, AUTHY_TEST_RP: relyingPartyOrigin }, stdout: "inherit", stderr: "inherit",
  });
  const result = await child.exited;
  if (result !== 0) process.exitCode = result;
} finally { await server?.close(); callback.stop(true); }
