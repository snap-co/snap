import { useEffect, useRef, useState } from "react";
import type { Client } from "@snap/wasm";
import { TestyClient } from "../client";
import { WebChannel } from "../channel";

export type Calculator = {
  accumulator: string;
  history: { operation: string; operand: string; before: string; after: string }[];
};

/** Each mounted experiment owns one carrier; leaving its page discards it. */
export function useConnection(app: TestyClient, calculator: boolean) {
  const [status, setStatus] = useState("Connecting");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [calc, setCalc] = useState<Calculator>({ accumulator: "0", history: [] });
  const [frames, setFrames] = useState<string[]>([]);
  const [bearer, setBearer] = useState(sessionStorage.getItem("testy.session") || "");
  const client = useRef<Client | undefined>(undefined);
  const channel = useRef<WebChannel | undefined>(undefined);
  const id = useRef(crypto.randomUUID());
  const active = useRef(0);
  const retired = useRef(new Set<Client>());
  const reset = () => setCalc({ accumulator: "0", history: [] });
  function release() {
    const sdk = client.current;
    client.current = undefined;
    if (!sdk) return;
    // Async Wasm methods borrow Client until their promises settle. Closing the
    // carrier rejects pending IO; free only after the enclosing work has unwound.
    if (active.current) retired.current.add(sdk); else sdk.free();
  }

  async function connect() {
    const previous = channel.current;
    channel.current = undefined;
    previous?.dispose();
    release();
    setStatus("Connecting");
    const next = new WebChannel(
      frame => setFrames(old => [...old.slice(-99), frame]),
      () => { if (channel.current === next) { setStatus("Disconnected"); reset(); } },
    );
    channel.current = next;
    await next.ready;
    if (channel.current !== next) return;
    const sdk = app.create(next);
    client.current = sdk;
    if (calculator) {
      const session = sessionStorage.getItem("testy.session");
      if (!session) { setStatus("Sign in required"); return; }
      sdk.use_session(session);
      id.current = crypto.randomUUID();
      await sdk.start(id.current);
      setCalc(JSON.parse(await sdk.inspect()));
    }
    setStatus("Connected");
  }
  async function run(work: () => Promise<void>) {
    active.current++;
    setBusy(true); setError("");
    try { await work(); }
    catch (e) {
      setError(String(e));
      if (String(e).includes("InvalidBearer")) {
        sessionStorage.removeItem("testy.session"); setBearer("");
        setStatus("Sign in required"); reset();
      }
    } finally {
      active.current--;
      if (!active.current) {
        for (const sdk of retired.current) sdk.free();
        retired.current.clear();
      }
      setBusy(false);
    }
  }
  async function authenticate(enroll: boolean, email: string, password: string) {
    // Use a new unauthenticated physical channel for every explicit login attempt.
    sessionStorage.removeItem("testy.session");
    await connect();
    const token = await client.current!.authenticate(enroll, email, password);
    sessionStorage.setItem("testy.session", token); setBearer(token);
    id.current = crypto.randomUUID();
    await client.current!.start(id.current);
    setCalc(JSON.parse(await client.current!.inspect())); setStatus("Connected");
  }
  useEffect(() => {
    void run(connect);
    return () => {
      const previous = channel.current;
      channel.current = undefined;
      previous?.dispose();
      release();
    };
  }, [app, calculator]);
  return { status, setStatus, error, busy, calc, setCalc, reset, frames, bearer, setBearer, client, channel, id, connect, run, authenticate };
}
