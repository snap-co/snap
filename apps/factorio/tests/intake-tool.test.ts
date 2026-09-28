import { test, expect } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";

test("intake tools use account WebSocket credentials after shell environment loss", async () => {
  const directory = await mkdtemp("/tmp/opencode/intake-tool-");
  const requests: unknown[] = [];
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch(request,server) {
    if (new URL(request.url).pathname === "/transport" && server.upgrade(request)) return;
    if (request.headers.get("authorization") !== "Bearer fixture-account-token") return new Response("unauthorized", { status: 401 });
    return Response.json({identified:true,owner:"fixture",human:false});
  }, websocket:{message(socket,message){
    const frame=JSON.parse(String(message));
    if(frame.Connect){expect(frame.Connect.bearer).toBe("fixture-account-token");socket.send(JSON.stringify({Attached:{resumed:false}}));return;}
    const call=frame.Invoke;
    let value:unknown={config:{modules:{}},tickets:{},sessions:{},intakes:{}};
    if(call.operation.startsWith("factorio.intake-")){requests.push({operation:call.operation,input:call.input});value={revision:requests.length};}
    socket.send(JSON.stringify({Events:[{Accepted:{id:call.id}},{Completed:{id:call.id,outcome:{Ok:value}}}]}));
  }} });
  try {
    const config = join(directory, "tool.json");
    await writeFile(config, JSON.stringify({ origin: `http://127.0.0.1:${server.port}`, token: "fixture-account-token",workspace:"workspace" }), { mode: 0o600 });
    for (const command of ["intake-read", "intake-save"]) {
      const child = Bun.spawn([process.execPath, join(import.meta.dir, "../cli.ts"), command, ...(command === "intake-save" ? ["-"] : []), "--credentials", config,"--intake","intake"], { env: {}, stdin: new Blob([JSON.stringify({ revision: 1, route: "grill", rationale: "Needs scope", tickets: [] })]), stdout: "pipe", stderr: "pipe" });
      const error = await new Response(child.stderr).text();
      expect(await child.exited, error).toBe(0);
    }
    expect(requests).toEqual([{operation:"factorio.intake-read",input:{workspace:"workspace",id:"intake"}},{operation:"factorio.intake-drafts",input:{workspace:"workspace",id:"intake",drafts:{revision:1,route:"grill",rationale:"Needs scope",tickets:[]}}}]);
  } finally { server.stop(true); await rm(directory, { recursive: true, force: true }); }
});
