import { expect, test } from "bun:test";
import { clientOrigins, localIPv4Hosts, originsFor, publicOrigin, requestOrigin } from "../../scripts/dev-network";

test("development origin policy admits exact local authorities, never cross-origin requests", () => {
  const origins = originsFor(["127.0.0.1", "192.168.1.2", "100.64.1.2", "host.tail.test"], 3852);
  for (const origin of origins) {
    const host = new URL(origin).host;
    expect(requestOrigin(origins, { host })).toBe(origin);
    expect(requestOrigin(origins, { host, origin })).toBe(origin);
    expect(requestOrigin(origins, { host, origin: "null" })).toBeUndefined();
    expect(requestOrigin(origins, { host, origin: "https://evil.test" })).toBeUndefined();
  }
  expect(requestOrigin(origins, { host: "host.tail.test.evil.test:3852" })).toBeUndefined();
  expect(requestOrigin(origins, { host: "192.168.1.2:3853" })).toBeUndefined();
  expect(requestOrigin(origins, { host: "192.168.1.2:3852", origin: origins[0] })).toBeUndefined();
});

test("client callback origins share discovered hosts and preserve configured client ports", () => {
  expect(clientOrigins(["127.0.0.1", "100.64.1.2"], [{ id: "factorio", origin: "http://host.tail.test:7777" }]).factorio).toEqual([
    "http://127.0.0.1:7777", "http://100.64.1.2:7777", "http://host.tail.test:7777",
  ]);
  expect(localIPv4Hosts({})).toEqual(["127.0.0.1", "localhost"]);
  for (const app of ["chatty", "factorio"] as const) {
    for (const canonical of ["http://192.168.1.2", "http://192.168.1.2:80"]) {
      expect(clientOrigins(["127.0.0.1", "192.168.1.2"], [{ id: app, origin: canonical }])[app]).toEqual([
        "http://127.0.0.1", "http://192.168.1.2",
      ]);
    }
  }
  for (const value of ["http://0.0.0.0:3852", "http://host/path", "http://user@host", "ftp://host"]) expect(() => publicOrigin(value)).toThrow();
});

test("TLS proxy policy matches the exact configured authority and scheme", () => {
  const origin = "https://factorio.cc.example.test";
  expect(publicOrigin(origin).origin).toBe(origin);
  expect(requestOrigin([origin], { host: "factorio.cc.example.test", origin })).toBe(origin);
  expect(requestOrigin([origin], { host: "factorio.cc.example.test" })).toBe(origin);
  for (const value of ["http://factorio.cc.example.test", "https://chatty.cc.example.test", "null"]) {
    expect(requestOrigin([origin], { host: "factorio.cc.example.test", origin: value })).toBeUndefined();
  }
  expect(requestOrigin([origin], { host: "factorio.cc.example.test.evil.test" })).toBeUndefined();
  expect(clientOrigins(["127.0.0.1"], [{ id: "factorio", origin }]).factorio).toEqual([origin]);
  expect(clientOrigins(["127.0.0.1"], [], "cc.example.test")).toEqual({});
});
