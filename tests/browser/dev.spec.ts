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
  } finally {
    await page.goto("about:blank");
    await server.close();
  }
});
