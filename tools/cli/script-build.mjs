// App-declared host helpers are packaged beside the deployment configuration.
const [source, output, name, target, mode] = process.argv.slice(2);
const result = await Bun.build({ entrypoints: [source], outdir: output, target,
  minify: mode === "production", naming: name,
  define: { "process.env.NODE_ENV": JSON.stringify(mode) } });
if (!result.success) throw new AggregateError(result.logs, "Script build failed");
