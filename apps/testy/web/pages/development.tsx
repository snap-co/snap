import { useEffect, useRef, useState } from "react";
import { DevelopmentChannel, type Host, type DebuggerStatus } from "../development";

export function Development() {
  const [host, setHost] = useState<Host>();
  const [error, setError] = useState("");
  const [ceiling, setCeiling] = useState("1000");
  const [status, setStatus] = useState<DebuggerStatus>("Connecting");
  const channel = useRef<DevelopmentChannel | undefined>(undefined);
  useEffect(() => {
    const connection = new DevelopmentChannel(setHost, setStatus);
    channel.current = connection;
    return () => connection.dispose();
  }, []);
  async function act(action: object, input?: string) {
    try {
      setError("");
      await channel.current!.command(action, input);
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <section className="developer">
      <div className="section-heading">
        <span className="eyebrow">HOST CONTROLS</span>
        <span className={`pill ${host?.manual ? "held" : ""}`}>
          {host?.manual ? "Manual stepping" : "Running"}
        </span>
      </div>
      <h2>Execution desk</h2>
      <p className="muted">
        Shared by this screen and agents at <code>/__dev/ws</code>. Steps stop
        between application entries.
      </p>
      <p className="muted small" role="status">
        Debugger: {status}
        {status !== "Live" && host ? " · showing last report" : ""}
      </p>
      <fieldset className="development-controls" disabled={status !== "Live"}>
        <div className="controls">
          <button onClick={() => act({ action: "mode", manual: !host?.manual })}>
            {host?.manual ? "Run" : "Hold"}
          </button>
          <button onClick={() => act({ action: "step" })}>Step once</button>
          <label className="check">
            <input type="checkbox" checked={host?.breakpoint ?? false} onChange={e => act({ action: "breakpoint", enabled: e.target.checked })} />
            Break after acceptance
          </label>
        </div>
        <div className="execution-status" aria-live="polite">
          {host?.active ? <>
            <strong>{host.active.operation}</strong>
            <span>Ticket {host.active.ticket} · {host.active.waiting ? `needs ${host.active.waiting}` : host.active.accepted ? "ready for attempt" : "awaiting admission"}</span>
          </> : <><strong>Idle</strong><span>{host?.queued.length ?? 0} queued operations</span></>}
        </div>
        {host?.active?.waiting && <div className="controls">
          <input aria-label="Dependency value" value={ceiling} onChange={e => setCeiling(e.target.value)} />
          <button onClick={() => act({ action: "supply", ticket: host.active!.ticket, key: host.active!.waiting }, ceiling)}>Supply input</button>
          <button onClick={() => act({ action: "fail", ticket: host.active!.ticket, key: host.active!.waiting })}>Fail input</button>
        </div>}
        <div className="controls">
          <button onClick={() => act({ action: "snapshot" })}>Save state</button>
          <button disabled={!host?.snapshot} onClick={() => act({ action: "restore" })}>Restore state</button>
          <select aria-label="Program variant" value={host?.program ?? "standard"} onChange={e => act({ action: "replace", program: e.target.value })}>
            <option value="standard">Standard addition</option><option value="double-add">Double-add variant</option>
          </select>
        </div>
        <p className="muted small">
          Save, restore and replace require an idle executor. Replacement
          selects compiled code. To replay, restore and submit the calculation again.
        </p>
        {error && <p role="alert" className="error">{error}</p>}
      </fieldset>
      <details><summary>Committed records</summary><pre data-testid="host-state">{host?.states}</pre></details>
      <details><summary>Execution trace <span className="muted">last 256 observations</span></summary><pre data-testid="host-trace">{host?.trace}</pre></details>
    </section>
  );
}
