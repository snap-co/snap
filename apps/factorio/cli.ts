import { Factorio, randomID } from "./client";
const args = process.argv.slice(2);
const origin = process.env.FACTORIO_ORIGIN ?? "http://127.0.0.1:3852";
const client = new Factorio(origin, process.env.FACTORIO_TOKEN);
function option(name: string, fallback = "") { const i = args.indexOf(`--${name}`); if (i < 0) return fallback; const value = args[i + 1]; if (!value || value.startsWith("--")) throw new Error(`--${name} needs a value`); return value; }
function list(name: string) { return option(name).split(",").filter(Boolean); }
try {
  const [command, id] = args;
  if (!command || command === "help") {
    console.log(`factory login | status | ticket <JSON-file> | delete-ticket <id>
factory start --id <id> --modules <crate,...|*> [--tickets <id,...>] [--conversation <ses_id>] -- <intent>
factory publish <id> --evidence <file> [--findings <JSON-file>]
factory expand <id> --modules <crate,...|*>
factory intake -- <description> | intake --resume <intake-id>
factory intake-read | intake-save <JSON-file|->  (OpenCode intake tool)
factory accept|recover|cleanup|abandon|path <id>
FACTORIO_ORIGIN selects the host. FACTORIO_TOKEN is an agent token created in the authenticated browser.
Acceptance requires a human to approve the exact candidate in the browser first.`);
  } else if (command === "login") {
    console.log(`Open ${origin}/auth/login, then create an agent token. Set FACTORIO_TOKEN in your shell. The token cannot approve candidates.`);
  } else if (command === "intake-read" || command === "intake-save") {
    const configPath = option("intake-config");
    const config = configPath ? await Bun.file(configPath).json() as { origin: string; token: string } : undefined;
    const token = config?.token ?? process.env.FACTORIO_INTAKE_TOKEN;
    if (!token) throw new Error("This tool requires a Factorio intake session");
    const body = command === "intake-read" ? { action: "read" } : id === "-" ? await Bun.stdin.json() : await Bun.file(id!).json();
    const response = await fetch(`${config?.origin ?? origin}/api/intake-tool`, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(body) });
    const value = await response.json();
    if (!response.ok) throw new Error(value.error_description ?? "Draft rejected. Reread and reconcile.");
    console.log(JSON.stringify(value, null, 2));
  } else {
    if (!process.env.FACTORIO_TOKEN) throw new Error("Run factory login and set FACTORIO_TOKEN");
    let value: unknown;
    if (command === "intake") {
      let intake;
      if (option("resume")) {
        intake = (await client.workspace()).intakes?.[option("resume")];
        if (!intake) throw new Error("Intake not found");
        await client.intakeAction(intake.id, { action: "resume" });
      } else {
        const divider = args.indexOf("--");
        if (divider < 0 || !args[divider + 1]) throw new Error("Supply -- <description>");
        const id = `intake-${randomID()}`;
        console.error(`Intake ${id}. Resume with factory intake --resume ${id}`);
        intake = await client.intake(id, args.slice(divider + 1).join(" "));
      }
      // Reuse OpenCode's terminal UI, history and input handling.
      const child = Bun.spawn([process.env.FACTORIO_OPENCODE ?? "opencode", "--session", intake.conversation], { stdin: "inherit", stdout: "inherit", stderr: "inherit" });
      process.exitCode = await child.exited;
      process.exit(process.exitCode);
    } else if (command === "status") value = await client.workspace();
    else if (command === "path") { const w = await client.workspace(); if (!w.sessions[id!]) throw new Error("Session not found"); value = w.sessions[id!]!.worktree; }
    else if (command === "ticket") value = await client.command({ command: "ticket", ticket: await Bun.file(id!).json() });
    else if (command === "delete-ticket") value = await client.command({ command: "delete_ticket", id });
    else if (command === "start") {
      const divider = args.indexOf("--");
      if (divider < 0) throw new Error("Declare scope and supply -- <intent>");
      const conversation = option("conversation");
      const session = option("id", crypto.randomUUID());
      const w = await client.command({ command, id: session, prompt: args.slice(divider + 1).join(" "), tickets: list("tickets"), modules: list("modules"), ...(conversation ? { conversation } : {}) });
      value = { session: w.sessions[session], next: "Move this OpenCode conversation to the worktree before editing. Publish evidence when ready. Human approval is required before accept." };
    } else if (command === "publish") {
      const evidence = option("evidence"); if (!evidence) throw new Error("Supply --evidence <file>");
      value = await client.command({ command, id, evidence: await Bun.file(evidence).text(), findings: option("findings") ? await Bun.file(option("findings")).json() : [] });
    } else if (command === "expand") value = await client.command({ command, id, modules: list("modules") });
    else if (["accept", "recover", "cleanup", "abandon"].includes(command)) value = await client.command({ command, id });
    else throw new Error("Unknown command; run factory help");
    console.log(typeof value === "string" ? value : JSON.stringify(value, null, 2));
  }
} catch (error) { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; }
