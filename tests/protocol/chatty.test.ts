import { test, expect } from "bun:test";
import { chattyServer, BrowserSession } from "../adapters/chatty";

for (const host of ["native", "workers"] as const) {
test(`${host} concurrent duplicate sends return the same receipt and run the model once`, async () => {
  const server = await chattyServer({ host }); const browser = new BrowserSession(server.baseUrl, server.authy);
  try {
    await browser.login("duplicates@chatty.test");
    const thread = await (await browser.post("/api/thread/create", {})).json();
    const body = { thread_id: thread.id, message: "hold", request_id: "same-receipt" };
    const responses = await Promise.all(Array.from({ length: 16 }, () => browser.post("/api/send", body)));
    expect(responses.every(r => r.status === 202)).toBe(true);
    const receipts = await Promise.all(responses.map(r => r.json()));
    expect(new Set(receipts.map(r => r.turn_id)).size).toBe(1);
    const deadline = Date.now() + 5000;
    while (!server.model!.active() && Date.now() < deadline) await new Promise(done => setTimeout(done, 20));
    expect(server.model!.requests).toHaveLength(1);
    const conflicts = await Promise.all([browser.post("/api/send", { ...body, message: "changed" }), browser.post("/api/send", { ...body, request_id: "unrelated" })]);
    expect(conflicts.map(r => r.status)).toEqual([409, 409]);
    server.model!.release(); expect((await browser.settled(thread.id)).turns).toHaveLength(1);
  } finally { await server.close(); }
}, 120_000);
test(`${host} Chatty reserves four retained generation slots across concurrent threads`, async () => {
  const server = await chattyServer({ host }); const browser = new BrowserSession(server.baseUrl, server.authy);
  try {
    await browser.login("capacity@chatty.test");
    const threads: string[] = [];
    for (let i = 0; i < 16; i++) threads.push((await (await browser.post("/api/thread/create", {})).json()).id);
    const replies = await Promise.all(threads.map(thread_id => browser.post("/api/send", { thread_id, message: "hold", request_id: "admission" })));
    expect(replies.filter(r => r.status === 202)).toHaveLength(4);
    expect(replies.filter(r => r.status === 503)).toHaveLength(12);
    const admitted = threads.filter((_, i) => replies[i].status === 202);
    const deadline = Date.now() + 5000;
    while (server.model!.active() < 4 && Date.now() < deadline) await new Promise(done => setTimeout(done, 20));
    expect(server.model!.active()).toBe(4); expect(server.model!.peak()).toBe(4);
    server.model!.release();
    for (const id of admitted) expect((await browser.settled(id)).turns[0].status).toBe("complete");
    const response = await browser.post("/api/send", { thread_id: admitted[0], message: "capacity returned", request_id: "next" });
    expect(response.status).toBe(202); expect((await browser.settled(admitted[0])).turns.at(-1).status).toBe("complete");
    expect(server.model!.peak()).toBe(4);
  } finally { await server.close(); }
}, 120_000);
test(`${host} Chatty uses Authy login, isolates persistent threads and replays reasoning`, async () => {
  const server = await chattyServer({ host }); const alice = new BrowserSession(server.baseUrl, server.authy); const bob = new BrowserSession(server.baseUrl, server.authy);
  try {
    const session = await alice.login("alice@chatty.test"); expect(session.account.email).toBe("alice@chatty.test");
    expect(JSON.stringify(session)).not.toContain("access_token");
    await bob.login("bob@chatty.test");
    const created = await alice.post("/api/thread/create", { effort: "low" }); expect(created.status).toBe(200); const thread = await created.json();
    expect((await bob.fetch(`/api/thread?id=${thread.id}`)).status).toBe(404);
    expect((await bob.post("/api/thread/delete", { thread_id: thread.id })).status).toBe(404);
    const post = (message: string, request_id = crypto.randomUUID()) => alice.post("/api/send", { thread_id: thread.id, message, request_id });
    expect((await post("first question", "same-request")).status).toBe(202);
    let view = await alice.settled(thread.id); expect(view.turns[0]).toMatchObject({ status: "complete", text: "Reply to first question", summary: "A short supplied summary." });
    expect(view.turns[0].usage.reasoning_tokens).toBe(5); expect(JSON.stringify(view)).not.toContain("opaque-reasoning");
    expect((await post("first question", "same-request")).status).toBe(202); expect(server.model!.requests.length).toBe(1);
    expect((await post("different", "same-request")).status).toBe(409);
    await server.restart(); expect((await alice.view(thread.id)).turns[0].text).toBe("Reply to first question");
    expect((await post("second question")).status).toBe(202); await alice.settled(thread.id);
    const input = server.model!.requests[1].input; expect(input[1].encrypted_content).toBe("opaque-reasoning"); expect(input[2].phase).toBe("final_answer"); expect(input[3].content).toBe("second question");
    expect(server.model!.requests[1]).toMatchObject({ model: "muse-spark-1.3-contributor", store: false, include: ["reasoning.encrypted_content"] });
    if (host === "native") {
    await post("write: save note"); view = await alice.settled(thread.id); expect(view.turns.at(-1).tools[0].result.result.written).toBe(true);
    await post("read:notes/proof.txt"); view = await alice.settled(thread.id); expect(view.turns.at(-1).tools[0].result.result.content).toBe("private note");
    await post("read:../../chatty.sqlite"); view = await alice.settled(thread.id); expect(view.turns.at(-1).tools[0].result.ok).toBe(false);
    const other = await (await bob.post("/api/thread/create", {})).json(); await bob.post("/api/send", { thread_id: other.id, message: "read:notes/proof.txt", request_id: "bob-read" }); const otherView = await bob.settled(other.id); expect(otherView.turns[0].tools[0].result.ok).toBe(false);
    }
    await post("hold");
    expect((await post("overlap")).status).toBe(409);
    view = await alice.view(thread.id); const active = view.thread.active_turn;
    expect((await alice.post("/api/cancel", { thread_id: thread.id, turn_id: active })).status).toBe(200);
    server.model!.release(); view = await alice.settled(thread.id); expect(view.turns.at(-1).status).toBe("cancelled");
    await post("broken"); view = await alice.settled(thread.id); expect(view.turns.at(-1).status).toBe("failed");
    await post("terminal-kept-open"); view = await alice.settled(thread.id); expect(view.turns.at(-1).status).toBe("complete");
    await post("hold"); await server.restart(); server.model!.release(); view = await alice.view(thread.id); expect(view.turns.at(-1).status).toBe("interrupted"); expect(view.thread.active_turn).toBe("");
    const forbidden = await alice.fetch("/api/thread/create", { method: "POST", headers: { "content-type": "application/json", origin: "https://attacker.example", "x-chatty-csrf": alice.csrf }, body: "{}" }); expect(forbidden.status).toBe(403);
    const logout = await alice.post("/auth/logout", {}); expect(logout.status).toBe(200); expect((await (await alice.fetch("/api/session")).json()).identified).toBe(false);
    expect((await (await bob.fetch("/api/session")).json()).identified).toBe(true);
  } finally { await server.close(); }
}, 120_000);
}
