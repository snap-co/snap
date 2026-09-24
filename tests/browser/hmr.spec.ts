import { test, expect } from "@playwright/test";
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { healthyProject } from "../adapters/project";
import { startServer } from "../adapters/server";

test("React refresh and CSS updates preserve page, component, and Rust client state", async ({ page }) => {
  test.setTimeout(180_000);
  const project = await healthyProject();
  const renderer = resolve(project.directory, "web/health-monitor.tsx");
  const original = (await readFile(renderer, "utf8"))
    .replace("{ useSyncExternalStore }", "{ useState, useSyncExternalStore }")
    .replace("  const { status, samples }", "  const [count, setCount] = useState(0);\n  const { status, samples }")
    .replace("<h1>Healthy</h1>", '<h1>Healthy</h1><button className="counter" onClick={() => setCount(count + 1)}>Count {count}</button>');
  await writeFile(renderer, original);
  let server: Awaited<ReturnType<typeof startServer>> | undefined;
  try {
    server = await startServer({ dev: true, project: project.directory, env: project.env });
    await page.goto(server.baseUrl);
    await expect(page.getByRole("status")).toHaveText("OK");
    await page.getByRole("button", { name: "Count 0" }).click();
    await page.evaluate(() => { (window as any).pageLifetime = "unchanged"; });
    const build = await (await fetch(`${server.baseUrl}/__snap/build`)).json();
    const firstSample = await page.locator(".sample").first().getAttribute("title");
    await writeFile(renderer, original.replace("<h1>Healthy</h1>", "<h1>Refreshed Healthy</h1>"));
    await expect(page.getByRole("heading", { name: "Refreshed Healthy" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Count 1" })).toBeVisible();
    const css = resolve(project.directory, "web/style.css");
    await writeFile(css, `${await readFile(css, "utf8")}\n.counter { color: rgb(12, 34, 56); }\n`);
    await expect(page.locator(".counter")).toHaveCSS("color", "rgb(12, 34, 56)");
    expect(await page.evaluate(() => (window as any).pageLifetime)).toBe("unchanged");
    expect(await page.locator(".sample").first().getAttribute("title")).toBe(firstSample);
    expect(await (await fetch(`${server.baseUrl}/__snap/build`)).json()).toEqual(build);
    expect(server.backendUrl).toBeDefined();
  } finally {
    await page.goto("about:blank");
    await server?.close();
    await project.close();
  }
  await expect(fetch(`${server!.baseUrl}/health/up`)).rejects.toThrow();
  await expect(fetch(`${server!.backendUrl}/health/up`)).rejects.toThrow();
});
