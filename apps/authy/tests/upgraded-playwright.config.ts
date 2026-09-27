import { defineConfig } from "@playwright/test";

export default defineConfig({ testDir: "browser", testMatch: "upgrade.spec.ts", workers: 1,
  timeout: 30000, use: { baseURL: process.env.AUTHY_TEST_URL ?? "http://127.0.0.1:3846" },
});
