// Contract fixture: forwards the native adapter's pipe protocol to the owned
// disposable HTTP fixture. It never discovers or contacts a real OpenCode.
export {};
const input = await Bun.stdin.json();
const url = process.env.FACTORIO_FIXTURE_API!;
const response = await fetch(input.watch ? `${url}/opencode/watch/${input.watch}` : `${url}/opencode/request`, input.watch ? undefined : { method: "POST", body: JSON.stringify(input) });
if (!response.ok) process.exit(1);
if (input.watch) { for await (const chunk of response.body!) await Bun.write(Bun.stdout, chunk); }
else console.log(await response.text());
