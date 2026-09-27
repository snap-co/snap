import { defineConfig } from "@playwright/test";
export default defineConfig({ testDir: "browser", workers: 1, timeout: 30000, use: { baseURL: process.env.CHATTY_TEST_URL } });
