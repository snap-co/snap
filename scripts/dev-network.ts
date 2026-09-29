import { networkInterfaces } from "node:os";
import { lookup } from "node:dns/promises";

export function localIPv4Hosts(interfaces = networkInterfaces()) {
  return [...new Set(["127.0.0.1", "localhost", ...Object.values(interfaces).flatMap(entries =>
    (entries ?? []).filter(e => e.family === "IPv4" && e.address !== "0.0.0.0").map(e => e.address))])];
}

// Discover once at startup. No request-supplied hostname can expand this list.
export async function devHosts() {
  const hosts = new Set(localIPv4Hosts());
  const tailscale = Bun.which("tailscale");
  if (tailscale) {
    const child = Bun.spawn([tailscale, "status", "--json"], { stdout: "pipe", stderr: "ignore" });
    const timeout = setTimeout(() => child.kill(), 2000);
    try {
      const status = await new Response(child.stdout).json();
      if (await child.exited === 0) {
        const name = status.Self?.DNSName?.replace(/\.$/, "");
        for (const candidate of [name, name?.split(".")[0]]) {
          if (!candidate) continue;
          const addresses = await lookup(candidate, { all: true, family: 4 }).catch(() => []);
          if (addresses.some(a => hosts.has(a.address))) hosts.add(candidate);
        }
      }
    } catch { /* Tailscale is optional. Interface IPs still work. */ }
    finally { clearTimeout(timeout); }
  }
  return [...hosts];
}

export function originsFor(hosts: string[], port: string | number) {
  return hosts.map(host => new URL(`http://${host}:${port}`).origin);
}

export function requestOrigin(origins: readonly string[], headers: { host?: string; origin?: string }) {
  // The startup policy determines the scheme, never an incoming forwarding header.
  // HTTPS policies are served only behind the loopback proxy listener.
  return origins.find(origin => new URL(origin).host === headers.host &&
    (headers.origin === undefined || headers.origin === origin));
}

export function publicOrigin(value: string) {
  const url = new URL(value);
  if (!["http:", "https:"].includes(url.protocol) || url.pathname !== "/" || url.search || url.hash || url.username || url.password || ["0.0.0.0", "[::]"].includes(url.hostname)) {
    throw new Error("Development public URLs must be HTTP(S) origins with a reachable hostname or IP");
  }
  return url;
}

export function clientOrigins(hosts: string[], env: Record<string, string | undefined> = process.env) {
  // Authy's native configuration derives exact per-client HTTPS callbacks.
  if (env.AUTHY_APP_DOMAIN) return {};
  return Object.fromEntries(([ ["chatty", 3850], ["factorio", 3852] ] as const).map(([app, fallback]) => {
    const prefix = app.toUpperCase();
    const canonical = env[`${prefix}_ORIGIN`] ? publicOrigin(env[`${prefix}_ORIGIN`]!) : undefined;
    if (canonical?.protocol === "https:") return [app, [canonical.origin]];
    const listen = new URL(`http://${env[`${prefix}_WEB_ADDR`] ?? `0.0.0.0:${fallback}`}`);
    const port = canonical ? canonical.port || "80" : listen.port || "80";
    if (port === "0") throw new Error(`${prefix}_ORIGIN must supply the app's allocated port to Authy`);
    return [app, [...new Set([...originsFor(hosts, port), ...(canonical ? [canonical.origin] : [])])]];
  }));
}
