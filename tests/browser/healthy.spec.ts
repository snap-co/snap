import { test, expect } from "@playwright/test";
import { startServer } from "../adapters/server";
import { resolve } from "node:path";

test("packaged Healthy renders Rust observations and recovers after a failed poll", async ({
  page,
}) => {
  // No asset path injection: the packaged binary must find its adjacent web directory.
  const server = await startServer({
    executable: process.env.SNAP_CHECK_EXECUTABLE ?? resolve("tests/fixtures/healthy/.snap/build/release/healthy"),
  });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(server.baseUrl);
    await expect(
      page.getByRole("heading", { name: "Healthy", exact: true }),
    ).toBeVisible();
    await expect(page.getByRole("status")).toHaveText("OK");
    await expect(
      page.getByLabel("Health check history").locator(".sample.ok").first(),
    ).toBeVisible();
    await page.route("**/health/up", (route) => route.abort("failed"));
    await expect(page.getByRole("status")).toHaveText("NOT OK", {
      timeout: 10_000,
    });
    await expect(
      page.getByLabel("Health check history").locator(".sample.error").first(),
    ).toBeVisible();
    await page.unroute("**/health/up");
    await expect(page.getByRole("status")).toHaveText("OK", {
      timeout: 10_000,
    });
    expect(errors).toEqual([]);
    await page.goto("about:blank");
  } finally {
    await server.close();
  }
});
