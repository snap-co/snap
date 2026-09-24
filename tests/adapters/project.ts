import { cp, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

/** Each editing contract owns its source copy, bindings, outputs, and processes. */
export async function healthyProject() {
  const root = resolve(import.meta.dirname, "../..");
  await mkdir(resolve(root, ".tmp"), { recursive: true });
  const directory = await mkdtemp(resolve(root, ".tmp/healthy-edit-"));
  await cp(resolve(root, "apps/healthy/web"), resolve(directory, "web"), { recursive: true });
  const client = await readFile(resolve(root, "apps/healthy/client.ts"), "utf8");
  await writeFile(resolve(directory, "client.ts"), client.replaceAll(
    '"../../clients/typescript/src"', JSON.stringify(resolve(root, "clients/typescript/src")),
  ));
  await writeFile(resolve(directory, "snap.toml"), `
version=1
application="healthy-edit"
[server]
manifest=${JSON.stringify(resolve(root, "apps/healthy/native/Cargo.toml"))}
bin="healthy"
[web]
package-dir=${JSON.stringify(root)}
application="web/app.tsx"
host=${JSON.stringify(resolve(root, "clients/react/main.tsx"))}
html=${JSON.stringify(resolve(root, "clients/react/index.html"))}
wasm-manifest=${JSON.stringify(resolve(root, "apps/healthy/wasm/Cargo.toml"))}
bindings=".snap/bindings"
[dev]
address="127.0.0.1:0"
`);
  const tools = resolve(root, "apps/healthy/.snap/tools");
  const installed = await readdir(tools).catch(() => []);
  return {
    directory,
    env: { PATH: [...installed.map((tool) => resolve(tools, tool, "bin")), process.env.PATH].join(":") },
    close: () => rm(directory, { recursive: true, force: true }),
  };
}
