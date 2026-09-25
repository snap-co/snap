import { test, expect } from "bun:test";
import { workersServer } from "../adapters/workers";

test("Workers dispatches the existing Healthy application", async () => {
  const server = await workersServer("healthy");
  try {
    const response = await fetch(`${server.baseUrl}/health/up`, { headers: { "x-snap-operation-id": "worker-health" } });
    expect(await response.json()).toEqual({ key: "transport.complete", target: "worker-health", payload: { ok: true, payload: { status: "OK" } } });
    expect((await fetch(`${server.baseUrl}/health/up`, { method: "POST" })).status).toBe(405);
  } finally { await server.close(); }
}, 120_000);

test("Durable Object Store satisfies the shared contract and survives workerd restart", async () => {
  const server = await workersServer("contract");
  try {
    const response = await fetch(`${server.baseUrl}/contract`);
    expect(await response.text()).toBe("Store contract passed");
    const before = await (await fetch(`${server.baseUrl}/read`)).text();
    expect(before).toContain("Integer(25)");
    await server.restart();
    expect(await (await fetch(`${server.baseUrl}/read`)).text()).toBe(before);
  } finally { await server.close(); }
}, 120_000);
