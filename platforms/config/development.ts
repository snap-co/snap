import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { parse, stringify } from "smol-toml";

/** Development tooling materializes explicit runtime inputs. Servers never read
 * these tooling variables or inherit per-field environment overrides. */
export async function development(app: string, root: string) {
  const path = resolve(process.env.SNAP_CONFIG ?? `${root}/apps/${app}/.deployment/development/config.toml`);
  const config = parse(await readFile(path, "utf8")) as any;
  if (config.host?.mode !== "development") throw new Error("snap dev requires development configuration");
  config.host.data_dir = resolve(dirname(path), config.host.data_dir);
  config.host.web_dir = resolve(dirname(path), config.host.web_dir ?? "web");
  if (config.app?.tools?.bridge) config.app.tools.bridge = resolve(dirname(path), config.app.tools.bridge);
  let key = process.env.SNAP_MASTER_KEY;
  if (!key) {
    try { key = (await readFile(resolve(dirname(path), "secrets.key"), "utf8")).trim(); }
    catch (error: any) { if (error.code !== "ENOENT") throw error; }
  }
  let bag: Uint8Array | undefined;
  try { bag = await readFile(resolve(dirname(path), "secrets.enc")); }
  catch (error: any) { if (error.code !== "ENOENT") throw error; }
  return { config, key, bag };
}
export async function generation(directory: string, input: Awaited<ReturnType<typeof development>>, host: Record<string, unknown>) {
  const config = structuredClone(input.config);
  config.host = { ...config.host, ...host };
  for (const [name, value] of Object.entries(config.host)) if (value === undefined) delete config.host[name];
  delete config.dev;
  if (config.app?.tools && !config.app.tools.bridge) config.app.tools.bridge = resolve(directory, "bridge.js");
  await writeFile(`${directory}/config.toml`, stringify(config));
  if (input.bag) await writeFile(`${directory}/secrets.enc`, input.bag);
  return `${directory}/config.toml`;
}
