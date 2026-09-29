import React, { useEffect, useRef, useState } from "react";
import { Client } from "@snap/wasm";
import { WebChannel } from "./channel";
import {
  DevelopmentChannel,
  type Host,
  type DebuggerStatus,
} from "./development";
import "./style.css";

type Calculator = {
  accumulator: string;
  history: {
    operation: string;
    operand: string;
    before: string;
    after: string;
  }[];
};
function Development() {
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
          <button
            onClick={() => act({ action: "mode", manual: !host?.manual })}
          >
            {host?.manual ? "Run" : "Hold"}
          </button>
          <button onClick={() => act({ action: "step" })}>Step once</button>
          <label className="check">
            <input
              type="checkbox"
              checked={host?.breakpoint ?? false}
              onChange={(e) =>
                act({ action: "breakpoint", enabled: e.target.checked })
              }
            />
            Break after acceptance
          </label>
        </div>
        <div className="execution-status" aria-live="polite">
          {host?.active ? (
            <>
              <strong>{host.active.operation}</strong>
              <span>
                Ticket {host.active.ticket} ·{" "}
                {host.active.waiting
                  ? `needs ${host.active.waiting}`
                  : host.active.accepted
                    ? "ready for attempt"
                    : "awaiting admission"}
              </span>
            </>
          ) : (
            <>
              <strong>Idle</strong>
              <span>{host?.queued.length ?? 0} queued operations</span>
            </>
          )}
        </div>
        {host?.active?.waiting && (
          <div className="controls">
            <input
              aria-label="Dependency value"
              value={ceiling}
              onChange={(e) => setCeiling(e.target.value)}
            />
            <button
              onClick={() =>
                act(
                  {
                    action: "supply",
                    ticket: host.active!.ticket,
                    key: host.active!.waiting,
                  },
                  ceiling,
                )
              }
            >
              Supply input
            </button>
            <button
              onClick={() =>
                act({
                  action: "fail",
                  ticket: host.active!.ticket,
                  key: host.active!.waiting,
                })
              }
            >
              Fail input
            </button>
          </div>
        )}
        <div className="controls">
          <button onClick={() => act({ action: "snapshot" })}>
            Save state
          </button>
          <button
            disabled={!host?.snapshot}
            onClick={() => act({ action: "restore" })}
          >
            Restore state
          </button>
          <select
            aria-label="Program variant"
            value={host?.program ?? "standard"}
            onChange={(e) =>
              act({ action: "replace", program: e.target.value })
            }
          >
            <option value="standard">Standard addition</option>
            <option value="double-add">Double-add variant</option>
          </select>
        </div>
        <p className="muted small">
          Save, restore and replace require an idle executor. Replacement
          selects compiled code. To replay, restore and submit the calculation
          again.
        </p>
        {error && (
          <p role="alert" className="error">
            {error}
          </p>
        )}
      </fieldset>
      <details>
        <summary>Committed records</summary>
        <pre data-testid="host-state">{host?.states}</pre>
      </details>
      <details>
        <summary>
          Execution trace <span className="muted">last 256 observations</span>
        </summary>
        <pre data-testid="host-trace">{host?.trace}</pre>
      </details>
    </section>
  );
}
export function App() {
  const route = location.pathname;
  const [status, setStatus] = useState("Connecting");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [calc, setCalc] = useState<Calculator>({
    accumulator: "0",
    history: [],
  });
  const [operand, setOperand] = useState("10");
  const [health, setHealth] = useState("");
  const [frames, setFrames] = useState<string[]>([]);
  const [bearer, setBearer] = useState(sessionStorage.getItem("testy.session") || "");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const client = useRef<Client | undefined>(undefined);
  const channel = useRef<WebChannel | undefined>(undefined);
  const id = useRef(crypto.randomUUID());
  async function connect() {
    channel.current?.dispose();
    client.current?.free();
    client.current = undefined;
    setStatus("Connecting");
    const next = new WebChannel(
      (frame) => setFrames((old) => [...old.slice(-99), frame]),
      () => {
        if (channel.current === next) {
          setStatus("Disconnected");
          setCalc({ accumulator: "0", history: [] });
        }
      },
    );
    channel.current = next;
    await next.ready;
    const sdk = new Client(next);
    client.current = sdk;
    if (route === "/calc") {
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
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (e) {
      setError(String(e));
      if (String(e).includes("InvalidBearer")) {
        sessionStorage.removeItem("testy.session");
        setBearer("");
        setStatus("Sign in required");
        setCalc({ accumulator: "0", history: [] });
      }
    } finally {
      setBusy(false);
    }
  }
  async function authenticate(enroll: boolean) {
    // Use a new unauthenticated physical channel for every explicit login attempt.
    sessionStorage.removeItem("testy.session");
    await connect();
    const token = await client.current!.authenticate(enroll, email, password);
    sessionStorage.setItem("testy.session", token);
    setBearer(token);
    setPassword("");
    id.current = crypto.randomUUID();
    await client.current!.start(id.current);
    setCalc(JSON.parse(await client.current!.inspect()));
    setStatus("Connected");
  }
  useEffect(() => {
    if (route === "/calc" || route === "/healthy") void run(connect);
    return () => {
      channel.current?.dispose();
    };
  }, []);
  const isApp = route === "/calc" || route === "/healthy";
  return (
    <main>
      <header>
        <a className="brand" href="/">
          s<span>snap</span>
        </a>
        <span className="eyebrow">LOCAL PLAYGROUND</span>
        <span className="version">TESTY / 01</span>
      </header>
      {!isApp ? (
        <section className="launcher">
          <div className="intro">
            <span className="eyebrow">SMALL APPS. REAL CONTRACTS.</span>
            <h1>Your testing ground.</h1>
            <p>
              Open an app. Follow a request.
              <br />
              See what the host is doing.
            </p>
          </div>
          <div className="app-grid">
            <a className="app-tile" href="/healthy">
              <span className="app-icon health-icon">↗</span>
              <strong>Healthy</strong>
              <span>Check the connection</span>
            </a>
            <a className="app-tile" href="/calc">
              <span className="app-icon calc-icon">
                ＋<br />＝
              </span>
              <strong>Calculator</strong>
              <span>State over transport</span>
            </a>
          </div>
          <div className="launcher-note">
            <span className="status-dot" /> One host. A collection of small
            experiments.
          </div>
        </section>
      ) : (
        <>
          <nav>
            <a href="/">← All apps</a>
            <span className="pill">{status}</span>
          </nav>
          <div className="workspace">
            <section className="application">
              <span className="eyebrow">
                {route === "/calc" ? "CONNECTED STATE" : "ANONYMOUS REQUEST"}
              </span>
              <h1>{route === "/calc" ? "Calculator" : "Healthy"}</h1>
              {route === "/calc" ? (
                <>
                  <p className="muted">
                    Sign in to open a private calculator. Disconnecting or signing
                    out discards its value and history.
                  </p>
                  {!bearer && (
                    <fieldset disabled={busy}>
                      <label className="field">Email<input type="email" autoComplete="username" value={email} onChange={e => setEmail(e.target.value)} /></label>
                      <label className="field">Password<input type="password" autoComplete="current-password" value={password} onChange={e => setPassword(e.target.value)} /></label>
                      <div className="controls">
                        <button onClick={() => run(() => authenticate(false))}>Sign in</button>
                        <button onClick={() => run(() => authenticate(true))}>Create account</button>
                      </div>
                    </fieldset>
                  )}
                  <div className="display">
                    <span>COMMITTED VALUE</span>
                    <output data-testid="accumulator">
                      {calc.accumulator}
                    </output>
                  </div>
                  <label className="field">
                    Operand
                    <input
                      value={operand}
                      onChange={(e) => setOperand(e.target.value)}
                      inputMode="numeric"
                    />
                  </label>
                  <div className="operations">
                    {[
                      ["add", "+"],
                      ["sub", "−"],
                      ["mul", "×"],
                      ["div", "÷"],
                      ["add_checked", "Checked +"],
                    ].map(([operation, label]) => (
                      <button
                        key={operation}
                        disabled={busy || status !== "Connected"}
                        onClick={() =>
                          run(async () => {
                            const result = await client.current!.calculate(
                              operation,
                              operand,
                            );
                            setCalc((old) => ({ ...old, accumulator: result }));
                            setCalc(
                              JSON.parse(await client.current!.inspect()),
                            );
                          })
                        }
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                  <div className="controls">
                    <button
                      disabled={busy || status !== "Connected"}
                      onClick={() =>
                        run(async () => {
                           await client.current!.disconnect();
                           setStatus("Detached");
                           setCalc({ accumulator: "0", history: [] });
                        })
                      }
                    >
                      Disconnect
                    </button>
                    <button
                      disabled={busy || !bearer || status === "Connected"}
                      onClick={() => run(connect)}
                    >
                      Reconnect
                    </button>
                    <button
                      disabled={busy || status !== "Connected"}
                      onClick={() =>
                        run(async () => {
                          await client.current!.close();
                          setStatus("Closed");
                          setCalc({ accumulator: "0", history: [] });
                        })
                      }
                    >
                      Close calculator
                    </button>
                    <button
                      disabled={busy || status !== "Connected"}
                      onClick={() =>
                        run(async () =>
                          setCalc(JSON.parse(await client.current!.inspect())),
                        )
                      }
                    >
                      Refresh
                    </button>
                    {bearer && <button disabled={busy} onClick={() => run(async () => {
                      // Reconnect only the carrier if it was lost, never replay logout.
                      if (status !== "Connected") await connect();
                      await client.current!.logout();
                      sessionStorage.removeItem("testy.session");
                      setBearer("");
                      setStatus("Sign in required");
                      setCalc({ accumulator: "0", history: [] });
                      channel.current?.dispose();
                    })}>Sign out</button>}
                  </div>
                  <p className="muted small">
                    Client <code>{id.current}</code>
                  </p>
                  <h2>
                    History <span className="count">{calc.history.length}</span>
                  </h2>
                  <div className="history">
                    {calc.history.length ? (
                      calc.history
                        .slice()
                        .reverse()
                        .map((entry, i) => (
                          <div key={i}>
                            <span>
                              {entry.operation.replace("calc.", "")}{" "}
                              {entry.operand}
                            </span>
                            <span>
                              {entry.before} → <strong>{entry.after}</strong>
                            </span>
                          </div>
                        ))
                    ) : (
                      <p className="muted">
                        Your first calculation starts here.
                      </p>
                    )}
                  </div>
                </>
              ) : (
                <>
                  <p className="muted">
                    A small round trip through transport and execution. No
                    identity or calculator connection required.
                  </p>
                  <div className={`health-result ${health ? "ok" : ""}`}>
                    <span>{health ? "✓" : "↗"}</span>
                    <output>{health || "Ready to check"}</output>
                  </div>
                  <button
                    className="primary"
                    disabled={busy || status !== "Connected"}
                    onClick={() =>
                      run(async () =>
                        setHealth(
                          JSON.parse(await client.current!.health()).status,
                        ),
                      )
                    }
                  >
                    Check health
                  </button>
                  {status !== "Connected" && (
                    <button disabled={busy} onClick={() => run(connect)}>
                      Reconnect
                    </button>
                  )}
                </>
              )}
              {busy && (
                <p className="pending" role="status">
                  Request in progress. Host controls remain available.
                </p>
              )}
              {error && (
                <p role="alert" className="error">
                  {error}
                </p>
              )}
              <details className="wire">
                <summary>Transport activity</summary>
                <pre>{frames.join("\n\n")}</pre>
              </details>
            </section>
            <Development />
          </div>
        </>
      )}
      <footer>
        <span>SNAP / TESTY</span>
        <span>Host-owned state. Observable execution.</span>
      </footer>
    </main>
  );
}
