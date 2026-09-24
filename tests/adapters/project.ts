import { cp, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

/** Each editing contract owns its source copy, bindings, outputs, and processes. */
export async function healthyProject(rust = false) {
  const root = resolve(import.meta.dirname, "../..");
  await mkdir(resolve(root, ".tmp"), { recursive: true });
  const directory = await mkdtemp(resolve(root, ".tmp/healthy-edit-"));
  await cp(resolve(root, "apps/healthy/web"), resolve(directory, "web"), { recursive: true });
  const client = await readFile(resolve(root, "apps/healthy/client.ts"), "utf8");
  await writeFile(resolve(directory, "client.ts"), client.replaceAll(
    '"../../clients/typescript/src"', JSON.stringify(resolve(root, "clients/typescript/src")),
  ));
  if (rust) {
    await mkdir(resolve(directory, "rust"));
    for (const [name, source] of [["app", ""], ["native", "native"], ["wasm", "wasm"]]) {
      await cp(resolve(root, "apps/healthy", source, "src"), resolve(directory, "rust", name, "src"), { recursive: true });
      const manifest = (await readFile(resolve(root, "apps/healthy", source, "Cargo.toml"), "utf8"))
        .replace(/\[\[test\]\][\s\S]*?(?=\[lints\])/, "");
      await writeFile(resolve(directory, "rust", name, "Cargo.toml"), manifest);
    }
    const workspace = (await readFile(resolve(root, "Cargo.toml"), "utf8"))
      .replace(/members = \[[\s\S]*?\]/, 'members = ["app", "native", "wasm"]')
      .replace(/path = "([^"]+)"/g, (_, path) => `path = ${JSON.stringify(path === "apps/healthy" ? "app" : resolve(root, path))}`);
    await writeFile(resolve(directory, "rust/Cargo.toml"), workspace);
    await cp(resolve(root, "Cargo.lock"), resolve(directory, "rust/Cargo.lock"));
  }
  await writeFile(resolve(directory, "snap.toml"), `
version=1
application="healthy-edit"
[server]
manifest=${JSON.stringify(rust ? "rust/native/Cargo.toml" : resolve(root, "apps/healthy/native/Cargo.toml"))}
bin="healthy"
[web]
package-dir=${JSON.stringify(root)}
application="web/app.tsx"
host=${JSON.stringify(resolve(root, "clients/react/main.tsx"))}
html=${JSON.stringify(resolve(root, "clients/react/index.html"))}
wasm-manifest=${JSON.stringify(rust ? "rust/wasm/Cargo.toml" : resolve(root, "apps/healthy/wasm/Cargo.toml"))}
bindings=".snap/bindings"
[dev]
address="127.0.0.1:0"
`);
  const tools = resolve(root, "apps/healthy/.snap/tools");
  const installed = await readdir(tools).catch(() => []);
  return {
    directory,
    env: { PATH: [...installed.map((tool) => resolve(tools, tool, "bin")), process.env.PATH].join(":"), CARGO_TARGET_DIR: resolve(root, "target") },
    close: () => rm(directory, { recursive: true, force: true }),
  };
}
