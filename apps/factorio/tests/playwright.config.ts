import { defineConfig } from "@playwright/test";
export default defineConfig({ testDir: "./browser", workers: 1, timeout: 90000, use: { headless: true, trace: "retain-on-failure" }, reporter: "list", outputDir: "/tmp/opencode/factorio-playwright" });
