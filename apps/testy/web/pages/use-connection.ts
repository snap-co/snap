import { useEffect, useRef, useState } from "react";
import type { Client } from "@snap/wasm";
import { identity_fetch, identity_acquire, identity_enroll, identity_release } from "@snap/wasm";
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
  const [signedIn, setSignedIn] = useState(false);
  const client = useRef<Client | undefined>(undefined);
  const channel = useRef<WebChannel | undefined>(undefined);
  const id = useRef(crypto.randomUUID());
  const active = useRef(0);
  const retired = useRef(new Set<Client>());
  const generation = useRef(0);
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
    const attempt = ++generation.current;
    const previous = channel.current;
    channel.current = undefined;
    previous?.dispose();
    release();
    if (!calculator) { setStatus("Connected"); return; }
    setStatus("Connecting");
    const principal = JSON.parse(await identity_fetch());
    if (attempt !== generation.current) return;
    setSignedIn(Boolean(principal));
    if (!principal) { setStatus("Sign in required"); reset(); return; }
    const next = new WebChannel(
      frame => setFrames(old => [...old.slice(-99), frame]),
      () => { if (channel.current === next) { setStatus("Disconnected"); reset(); } },
    );
    channel.current = next;
    await next.ready;
    if (channel.current !== next) return;
    const sdk = app.create(next);
    client.current = sdk;
    id.current = crypto.randomUUID();
    await sdk.start(id.current);
    setCalc(JSON.parse(await sdk.inspect()));
    setStatus("Connected");
  }
  async function run(work: () => Promise<void>) {
    active.current++;
    setBusy(true); setError("");
    try { await work(); }
    catch (e) {
      setError(String(e));
      if (String(e).includes("InvalidBearer")) {
        setSignedIn(false);
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
    if (enroll) await identity_enroll(email, password);
    else await identity_acquire(email, password);
    await connect();
  }
  async function signOut() {
    await identity_release("current");
    channel.current?.dispose();
    channel.current = undefined;
    release();
    setSignedIn(false); setStatus("Sign in required"); reset();
  }
  useEffect(() => {
    void run(connect);
    return () => {
      generation.current++;
      const previous = channel.current;
      channel.current = undefined;
      previous?.dispose();
      release();
    };
  }, [app, calculator]);
  return { status, setStatus, error, busy, calc, setCalc, reset, frames, signedIn, client, id, connect, run, authenticate, signOut };
}
