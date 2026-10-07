import { useState } from "react";
import { TestyClient } from "../client";
import { Experiment } from "./experiment";
import { useConnection } from "./use-connection";
import { health as checkHealth } from "@snap/wasm";

export function HealthyPage({ client: app }: { client: TestyClient }) {
  const { status, error, busy, frames, connect, run } = useConnection(app, false);
  const [health, setHealth] = useState("");
  return <Experiment status={status} title="Healthy" eyebrow="ANONYMOUS REQUEST" busy={busy} error={error} frames={frames}>
    <p className="muted">A small round trip through transport and execution. No identity or calculator connection required.</p>
    <div className={`health-result ${health ? "ok" : ""}`}><span>{health ? "✓" : "↗"}</span><output>{health || "Ready to check"}</output></div>
    <button className="primary" disabled={busy || status !== "Connected"} onClick={() => run(async () => setHealth(JSON.parse(await checkHealth()).status))}>Check health</button>
    {status !== "Connected" && <button disabled={busy} onClick={() => run(connect)}>Reconnect</button>}
  </Experiment>;
}
