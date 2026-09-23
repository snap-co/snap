const result = Bun.spawnSync(
  [
    process.execPath,
    new URL("../node_modules/typescript/bin/tsc", import.meta.url).pathname,
    "--project",
    new URL("../tsconfig.json", import.meta.url).pathname,
  ],
  { stdin: "inherit", stdout: "inherit", stderr: "inherit" },
);
if (!result.success) throw new Error("Client TypeScript check failed");
