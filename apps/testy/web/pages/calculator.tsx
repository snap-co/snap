import { useState } from "react";
import { TestyClient } from "../client";
import { Experiment } from "./experiment";
import { useConnection } from "./use-connection";

export function CalculatorPage({ client: app }: { client: TestyClient }) {
  const { status, setStatus, error, busy, calc, setCalc, reset, frames, bearer, setBearer, client, channel, id, connect, run, authenticate } = useConnection(app, true);
  const [operand, setOperand] = useState("10");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  async function signIn(enroll: boolean) { await authenticate(enroll, email, password); setPassword(""); }
  return <Experiment status={status} title="Calculator" eyebrow="CONNECTED STATE" busy={busy} error={error} frames={frames}>
    <p className="muted">Sign in to open a private calculator. Disconnecting or signing out discards its value and history.</p>
    {!bearer && <fieldset disabled={busy}>
      <label className="field">Email<input type="email" autoComplete="username" value={email} onChange={e => setEmail(e.target.value)} /></label>
      <label className="field">Password<input type="password" autoComplete="current-password" value={password} onChange={e => setPassword(e.target.value)} /></label>
      <div className="controls"><button onClick={() => run(() => signIn(false))}>Sign in</button><button onClick={() => run(() => signIn(true))}>Create account</button></div>
    </fieldset>}
    <div className="display"><span>COMMITTED VALUE</span><output data-testid="accumulator">{calc.accumulator}</output></div>
    <label className="field">Operand<input value={operand} onChange={e => setOperand(e.target.value)} inputMode="numeric" /></label>
    <div className="operations">{[["add", "+"], ["sub", "−"], ["mul", "×"], ["div", "÷"], ["add_checked", "Checked +"]].map(([operation, label]) =>
      <button key={operation} disabled={busy || status !== "Connected"} onClick={() => run(async () => {
        const result = await client.current!.calculate(operation, operand);
        setCalc(old => ({ ...old, accumulator: result }));
        setCalc(JSON.parse(await client.current!.inspect()));
      })}>{label}</button>,
    )}</div>
    <div className="controls">
      <button disabled={busy || status !== "Connected"} onClick={() => run(async () => { await client.current!.disconnect(); setStatus("Detached"); reset(); })}>Disconnect</button>
      <button disabled={busy || !bearer || status === "Connected"} onClick={() => run(connect)}>Reconnect</button>
      <button disabled={busy || status !== "Connected"} onClick={() => run(async () => { await client.current!.close(); setStatus("Closed"); reset(); })}>Close calculator</button>
      <button disabled={busy || status !== "Connected"} onClick={() => run(async () => setCalc(JSON.parse(await client.current!.inspect())))}>Refresh</button>
      {bearer && <button disabled={busy} onClick={() => run(async () => {
        // Reconnect only the carrier if it was lost, never replay logout.
        if (status !== "Connected") await connect();
        await client.current!.logout();
        sessionStorage.removeItem("testy.session"); setBearer(""); setStatus("Sign in required"); reset(); channel.current?.dispose();
      })}>Sign out</button>}
    </div>
    <p className="muted small">Client <code>{id.current}</code></p>
    <h2>History <span className="count">{calc.history.length}</span></h2>
    <div className="history">{calc.history.length ? calc.history.slice().reverse().map((entry, i) =>
      <div key={i}><span>{entry.operation.replace("calc.", "")} {entry.operand}</span><span>{entry.before} → <strong>{entry.after}</strong></span></div>,
    ) : <p className="muted">Your first calculation starts here.</p>}</div>
  </Experiment>;
}
