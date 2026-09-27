import { test } from "bun:test";
import { authyServer } from "../support/authy";
import { nativeIdentity } from "../support/authy-native";
import { passwordSessions } from "../sdk/identity.contract";

test("native SDK wiring", async () => {
  const server = await authyServer();
  try { await passwordSessions(() => nativeIdentity(server.baseUrl)); }
  finally { await server.close(); }
}, 30_000);
