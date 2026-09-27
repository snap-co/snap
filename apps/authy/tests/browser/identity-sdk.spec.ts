import { test } from "@playwright/test";
import { authyServer } from "../support/authy";
import { browserIdentity } from "../support/authy-browser";
import { passwordSessions } from "../sdk/identity.contract";

  test("password/session SDK wiring through the browser", async ({ browser }) => {
    test.setTimeout(120_000);
    const server = await authyServer(true);
    try { await passwordSessions(() => browserIdentity(browser, server.baseUrl)); }
    finally { await server.close(); }
  });
