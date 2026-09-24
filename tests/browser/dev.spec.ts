import { test, expect } from "@playwright/test";
import { startServer } from "../adapters/server";

test("snap dev builds and serves the configured browser application", async ({
  page,
}) => {
  test.setTimeout(150_000);
  const server = await startServer({ dev: true });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(server.baseUrl);
    await expect(
      page.getByRole("heading", { name: "Healthy", exact: true }),
    ).toBeVisible();
    await expect(page.getByRole("status")).toHaveText("OK");
    expect(errors).toEqual([]);
    for (const method of ["GET", "OPTIONS"]) {
      const headers = { Host: "healthy.test", Origin: "http://healthy.test", Cookie: "probe=value" };
      const direct = await fetch(`${server.backendUrl}/health/up`, { method, headers });
      const publicResponse = await fetch(`${server.baseUrl}/health/up`, { method, headers });
      expect(publicResponse.status).toBe(direct.status);
      // Successful completions may carry generated correlation identifiers.
      if (method === "OPTIONS") expect(await publicResponse.text()).toBe(await direct.text());
    }
    const module = await fetch(`${server.baseUrl}/@vite/client`, { headers: { Host: "healthy.test" } });
    expect(module.status).toBe(200);
  } finally {
    await page.goto("about:blank");
    await server.close();
  }
});
