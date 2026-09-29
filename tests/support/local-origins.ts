import { networkInterfaces } from "node:os";

// Fixture installation inputs, independent of Snap's origin-discovery code.
export function localOrigins(port: string) {
  const hosts = new Set(["127.0.0.1", "localhost"]);
  for (const entries of Object.values(networkInterfaces())) {
    for (const entry of entries ?? []) {
      if (entry.family === "IPv4" && entry.address !== "0.0.0.0") hosts.add(entry.address);
    }
  }
  return [...hosts].map(host => new URL(`http://${host}:${port}`).origin);
}
