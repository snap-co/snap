import { defineConfig } from "@playwright/test";
import { resolve } from "node:path";
export default defineConfig({ testDir: "./browser", workers: 1, timeout: 90000, use: { headless: true, trace: "retain-on-failure" }, reporter: "list", outputDir: resolve(process.env.TMPDIR ?? "/tmp/opencode", "factorio-playwright") });
