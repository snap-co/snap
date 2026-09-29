import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { stringify } from "smol-toml";

const root = resolve(import.meta.dirname, "../..");
/** Real TOML and age bags through the production CLI, scoped to a disposable fixture. */
export async function deployment(directory: string, config: Record<string, any>, secrets?: Record<string, any>) {
  const input = `${directory}/.deployment/development`;
  await mkdir(input, { recursive: true });
  await writeFile(`${directory}/Cargo.toml`, "[package]\nname='fixture'\nversion='0.0.0'\n");
  await writeFile(`${directory}/snap.toml`, "version=1\napplication='fixture'\n");
  const path = `${input}/config.toml`;
  await writeFile(path, stringify({ version: 1, ...config }));
  let key: string | undefined;
  if (secrets) {
    async function run(action: string) {
      const child = Bun.spawn([`${root}/target/debug/snap`, "secrets", action], { cwd: directory, stdout: "ignore", stderr: "pipe" });
      if (await child.exited !== 0) throw new Error(await new Response(child.stderr).text());
    }
    if (!await Bun.file(`${input}/secrets.key`).exists()) await run("init");
    await writeFile(`${input}/secrets.toml`, stringify(secrets), { mode: 0o600 });
    await run("seal");
    key = (await readFile(`${input}/secrets.key`, "utf8")).trim();
  }
  return { path, env: { ...process.env, SNAP_MASTER_KEY: key } };
}
