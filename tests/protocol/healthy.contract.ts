// Protocol promises the SDK intentionally hides. Expectations do not use Rust codecs.
import assert from "node:assert/strict";

export async function healthyProtocol(baseUrl: string) {
  const request = (path: string, init: RequestInit = {}) =>
    fetch(new URL(path, baseUrl), {
      ...init,
      signal: AbortSignal.timeout(5_000),
    });
  const buildResponse = await request("/__snap/build");
  assert.equal(buildResponse.status, 200);
  assert.equal(buildResponse.headers.get("cache-control"), "no-store");
  const build = await buildResponse.json();
  assert.equal(build.contract, 1);
  assert.equal(typeof build.application, "string");
  assert.ok(build.application.length > 0);
  assert.equal(typeof build.build, "string");
  assert.ok(build.build.length > 0);
  const response = await request("/health/up", {
    headers: {
      "x-snap-operation-id": "smoke-correlation",
      "x-snap-build": "stale-build",
    },
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "private, no-store");
  assert.deepEqual(await response.json(), {
    key: "transport.complete",
    target: "smoke-correlation",
    payload: { ok: true, payload: { status: "OK" } },
  });
  const invalid = await request("/health/up?unexpected=true", {
    headers: { "x-snap-operation-id": "invalid-input" },
  });
  assert.equal(invalid.status, 400);
  const rejection = await invalid.json();
  assert.equal(rejection.key, "transport.complete");
  assert.equal(rejection.target, "invalid-input");
  assert.equal(rejection.payload.ok, false);
  assert.equal(rejection.payload.error._tag, "InvalidInputError");
  assert.equal((await request("/health/up", { method: "POST" })).status, 405);
  assert.equal(
    (await request("/health/up", {
      method: "HEAD",
      headers: { "x-snap-build": build.build },
    })).status,
    405,
  );
  assert.equal((await request("/unknown/operation")).status, 404);
  return build as { contract: 1; application: string; build: string };
}
