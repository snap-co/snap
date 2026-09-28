type FixtureSession = { data: Record<string, unknown>; environment: Record<string, string>; tool?: string; messages: Record<string, unknown>[]; forms: Record<string, unknown>[]; permissions: Record<string, unknown>[]; seen: Set<string> };
export function openCodeFixture() {
  const sessions = new Map<string, FixtureSession>();
  const watchers = new Map<string, Set<ReadableStreamDefaultController<Uint8Array>>>();
  function snapshot(s: FixtureSession) { return { messages: s.messages, forms: s.forms, permissions: s.permissions }; }
  function notify(id: string) { const s = sessions.get(id)!; for (const sink of watchers.get(id) ?? []) sink.enqueue(new TextEncoder().encode(JSON.stringify(snapshot(s)) + "\n")); }
  async function save(s: FixtureSession) {
    const tool = async (body: unknown) => {
      if (!s.tool) throw new Error("Missing explicit intake command");
      const command = (body as {action?:string}).action === "read" ? s.tool : s.tool.replace(" intake-read ", " intake-save - ");
      // OpenCode may lose overrides between calls. Exercise the actual supplied
      // command with neither PATH nor Factorio credentials in the environment.
      const child = Bun.spawn(["/bin/bash", "-c", command], {env:{},stdin:new Blob([JSON.stringify(body)]),stdout:"pipe",stderr:"pipe"});
      const output = await new Response(child.stdout).text();
      if (await child.exited) throw new Error("Fixture scoped CLI failed with an empty environment");
      return JSON.parse(output);
    };
    const state = await tool({ action: "read" });
     const id = `${state.intake.id}-navigation`;
    await tool({ revision: state.intake.revision, route: "implement", rationale: "The outcome and single-module scope are agreed.", tickets: [{ id, title: "Improve mobile navigation", description: "Make navigation usable on a phone. Acceptance: ticket links remain visible at 390px.", modules: [Object.keys(state.modules)[0]], status: "draft", notes: "", parent: null, blockers: [] }] });
    s.messages.push({ id: "msg_fixture_answer", role: "assistant", parts: [{ type: "text", text: "I saved a single-module draft. Review it and mark it ready when you want to start." }] });
  }
  return async function handle(request: Request): Promise<Response | undefined> {
    const path = new URL(request.url).pathname;
    if (path.startsWith("/opencode/watch/")) {
      const id = path.split("/").at(-1)!;
      if (!sessions.has(id)) return new Response("missing", { status: 404 });
      let controller: ReadableStreamDefaultController<Uint8Array>;
      return new Response(new ReadableStream<Uint8Array>({ start(c) { controller = c; const set = watchers.get(id) ?? new Set(); set.add(c); watchers.set(id, set); notify(id); }, cancel() { watchers.get(id)?.delete(controller); } }));
    }
    if (path !== "/opencode/request") return;
    const { method, path: api, body } = await request.json();
    const id = api.split("/")[3];
    if (api === "/api/session" && method === "POST") {
      sessions.set(body.id, { data: body, environment: {}, messages: [], forms: [], permissions: [], seen: new Set() });
      return Response.json({ data: body });
    }
    const s = sessions.get(id);
    if (method === "DELETE") { sessions.delete(id); for (const sink of watchers.get(id) ?? []) sink.close(); watchers.delete(id); return Response.json(null); }
    if (!s) return new Response("missing", { status: 404 });
    if (api.endsWith("/environment")) s.environment = body.variables;
    else if (api.endsWith("/prompt")) {
      if (!s.seen.has(body.id)) {
        s.seen.add(body.id);
        if (body.metadata?.factorio_initial) {
          s.tool = body.text.split("\n").find((line:string)=>line.startsWith("'") && line.includes(" intake-read --credentials "));
          s.messages.push({ id: body.id, role: "user", text: "Improve navigation on my phone" });
          s.messages.push({ id: "msg_fixture_question", role: "assistant", parts: [{ type: "text", text: "Which navigation outcome matters most?" }] });
          s.forms = [{ id: "frm_scope", title: "Clarify navigation", fields: [
            { key: "scope", type: "multiselect", title: "Scope", required: true, custom: true, options: [{value:"known",label:"Known scope"}], default:["other"] },
            { key: "outcome", type: "string", title: "Desired outcome", required: true, when:[{key:"scope",op:"eq",value:"other"}] },
            { key: "alternate", type: "string", title: "Alternate outcome", default:"stale default", when:[{key:"scope",op:"neq",value:"other"}] },
            { key: "inactive", type: "string", hidden: true, default:"must not submit", when:[{key:"scope",op:"neq",value:"other"}] },
            { key: "unanswered", type: "string", title: "Optional detail" },
            { key: "followup", type: "string", title: "Unanswered follow-up", required:true, when:[{key:"unanswered",op:"neq",value:"no"}] },
          ] }];
          s.permissions = [{ id: "per_read", action: "read", resources: ["crates/a"] }];
        } else {
          s.messages.push({ id: body.id, role: "user", text: body.text });
          s.messages.push({ id: `msg_reply_${s.seen.size}`, role: "assistant", parts: [{ type: "text", text: "Your additional context is recorded." }] });
        }
      }
    } else if (api.includes("/form/") && api.endsWith("/reply")) {
      if (!body.answer.outcome || JSON.stringify(body.answer.scope)!=='["other"]' || Object.keys(body.answer).some(k=>!["scope","outcome"].includes(k))) return new Response("invalid", { status: 400 });
      s.forms = []; await save(s);
    } else if (api.includes("/permission/") && api.endsWith("/reply")) s.permissions = [];
    notify(id);
    return Response.json({ data: s.data });
  };
}
