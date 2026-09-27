// Explicit paid-provider gate. Default app tests use the deterministic local model.
import { chromium } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { pair } from "../apps/chatty/tests/support/pair";

const configured: Record<string,string> = {};
try {
  for (const line of (await readFile(".snap/chatty.env", "utf8")).split("\n")) {
    const match = /^\s*(OPENCODE_API_KEY|EXA_API_KEY|CHATTY_MODEL|CHATTY_MODEL_ENDPOINT)=(.*)$/.exec(line);
    if (!match) continue;
    const value = match[2].trim(); configured[match[1]] = value.startsWith('"') ? JSON.parse(value) : value;
  }
} catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
const env = { ...configured, ...process.env };
if (!env.OPENCODE_API_KEY || !env.EXA_API_KEY) throw new Error("Live gate needs OPENCODE_API_KEY and EXA_API_KEY");
const server = await pair({ provider: { key: env.OPENCODE_API_KEY, search: env.EXA_API_KEY, model: env.CHATTY_MODEL, endpoint: env.CHATTY_MODEL_ENDPOINT } });
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
try {
  browser = await chromium.launch(); const page = await browser.newPage();
  await page.goto(server.base);
  await page.getByRole("link", { name: /Continue with Authy/ }).click();
  await page.getByRole("button", { name: "New here? Create account", exact: true }).click();
  await page.getByLabel("Email", { exact: true }).fill("live-fixture@example.test");
  await page.getByLabel("Password", { exact: true }).fill("temporary live fixture password");
  await page.getByRole("button", { name: "Create account", exact: true }).click();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await page.getByLabel("Thinking effort").selectOption("low");
  await page.getByLabel("Message Chatty").fill("Use web_search once to find the official Rust book. Reply with only its title and URL. Do not read or write any files.");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".turn-error") || (document.querySelector(".assistant-message .prose")?.textContent && !document.querySelector(".working")), undefined, { timeout: 360000 });
  if (await page.locator(".turn-error").count()) throw new Error("Live provider reply failed; no response payload retained");
  if (!await page.locator(".tool summary").filter({ hasText: "web search" }).count()) throw new Error("Live reply did not execute search");
  const records = await page.locator(".tool pre").allTextContents();
  const searches = records.map(record => JSON.parse(record)).filter(record => record.result?.result?.sources);
  if (!searches.length) {
    const status = records.map(record => JSON.parse(record).result?.error).find(error => typeof error === "string" && /^Search unavailable, HTTP \d+$/.test(error));
    throw new Error(status ?? "Live search did not produce source results; no payload retained");
  }
  const sources = new Set(searches.flatMap(record => record.result.result.sources.map((source: { url: string }) => source.url)));
  const links = await page.locator(".assistant-message .prose a").evaluateAll(nodes => nodes.map(node => (node as HTMLAnchorElement).href));
  if (!links.some(link => sources.has(link))) throw new Error("Live reply did not cite a returned source URL");
  console.log("Live Chatty passed: Authy OAuth, provider generation, Exa tool result, cited answer and Document delivery.");
} finally { await browser?.close(); await server.close(); }
