import { test } from "@playwright/test";
import { authyServer } from "../adapters/authy";
import { nativeIdentity } from "../adapters/authy-native";
import { browserIdentity } from "../adapters/authy-browser";
import { passwordSessions } from "../sdk/identity.contract";

for (const adapter of ["native", "browser"] as const) {
  test(`password/session SDK contract through ${adapter}`, async ({ browser }) => {
    test.setTimeout(120_000);
    const server = await authyServer(adapter === "browser");
    try { await passwordSessions(() => adapter === "native" ? nativeIdentity(server.baseUrl) : browserIdentity(browser, server.baseUrl)); }
    finally { await server.close(); }
  });
}
