import { expect, test } from "bun:test";
import { chromium, type Browser } from "@playwright/test";

test("an identity change withholds old loader data until new-account onboarding is ready", async () => {
  const build = await Bun.build({ entrypoints: [`${import.meta.dir}/session-fixture.tsx`], target: "browser", define: { "process.env.NODE_ENV": '"production"' } });
  if (!build.success) throw new AggregateError(build.logs);
  const script = build.outputs.find(output => output.path.endsWith(".js"))!;
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => new URL(request.url).pathname === "/fixture.js" ? new Response(script) : new Response('<div id="root"></div><script type="module" src="/fixture.js"></script>', { headers: { "content-type": "text/html" } }) });
  let browser: Browser | undefined;
  try {
    browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.port}`);
    await page.getByText("Owner A", { exact: true }).waitFor();
    await page.evaluate(() => (window as any).switchStart());
    await page.getByRole("heading", { name: "Opening your workspace" }).waitFor();
    await page.evaluate(() => (window as any).switchReady());
    await page.waitForFunction(() => document.documentElement.dataset.loader === "held");
    // Observe a rendered frame, not just the synchronous state transition.
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
    expect(await page.getByText("Owner A", { exact: true }).isVisible()).toBe(false);
    expect(await page.getByRole("heading", { name: "Opening your workspace" }).count()).toBe(1);
    await page.evaluate(() => (window as any).release());
    await page.getByRole("option", { name: "repository" }).waitFor({ state: "attached" });
    expect(await page.getByText("Owner B", { exact: true }).count()).toBe(1);
    await page.evaluate(() => (window as any).disconnect());
    expect(await page.getByRole("combobox", { name: "Repository" }).inputValue()).toBe("repository");
  } finally { await browser?.close(); server.stop(true); }
}, 15000);
