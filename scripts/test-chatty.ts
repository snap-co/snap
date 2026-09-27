import { pair } from "../apps/chatty/tests/support/pair";
const server = await pair();
try {
  const child = Bun.spawn(["bunx", "playwright", "test", "--config", "apps/chatty/tests/playwright.config.ts"], {
    env: { ...process.env, CHATTY_TEST_URL: server.base, CHATTY_FIXTURE_URL: server.fixtureURL }, stdout: "inherit", stderr: "inherit",
  });
  const result = await child.exited; if (result) process.exitCode = result;
} finally { await server.close(); }
