import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "browser",
  timeout: 30_000,
  workers: 1,
  use: { browserName: "chromium", headless: true, trace: "retain-on-failure" },
  outputDir: "../../../test-results/authy",
});
