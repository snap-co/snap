import { expect, test } from "bun:test";
import { Invocations } from "../client";

test("ACK, streamed progress, completion and duplicate replies share one channel", async () => {
  const sent: string[] = [];
  const progress: string[] = [];
  let sequence = 0;
  const calls = new Invocations((operation, input) => JSON.stringify({ Invoke: { id: ++sequence, operation, input } }), frame => sent.push(frame));
  const result = calls.invoke<number, string>("test", {}, value => progress.push(value));
  expect(calls.receive(JSON.stringify({ Events: [{ Accepted: { id: 1 } }] }))).toBe(true);
  calls.receive(JSON.stringify({ Events: [{ Progress: { id: 1, value: "working" } }] }));
  expect(progress).toEqual(["working"]);
  expect(sent).toHaveLength(1);
  const complete = JSON.stringify({ Events: [{ Completed: { id: 1, outcome: { Ok: 42 } } }] });
  calls.receive(complete);
  expect(await result).toBe(42);
  expect(calls.receive(complete)).toBe(true);
  calls.close();
});

test("a surviving logical connection reuses the invocation but a fresh one rejects unknown outcomes", async () => {
  const sent: string[] = [];
  const calls = new Invocations((operation, input) => JSON.stringify({ Invoke: { id: 1, operation, input } }), frame => sent.push(frame));
  const result = calls.invoke("test", {});
  const rejection = result.then(() => { throw new Error("Expected unknown outcome"); }, error => error);
  calls.receive(JSON.stringify({ Events: [{ Accepted: { id: 1 } }] }));
  calls.detached();
  calls.receive(JSON.stringify({ Attached: { resumed: true } }));
  expect(sent).toEqual([sent[0], sent[0]]);
  calls.detached();
  calls.receive(JSON.stringify({ Attached: { resumed: false } }));
  expect((await rejection).message).toContain("prior outcomes are unknown");
  expect(sent).toHaveLength(2);
});
