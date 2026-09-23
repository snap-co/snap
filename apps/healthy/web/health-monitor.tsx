import { useSyncExternalStore } from "react";
import type { HealthyClient } from "../../../clients/typescript/src";

export function HealthMonitor({ client }: { client: HealthyClient }) {
  const { status, samples } = useSyncExternalStore(
    client.subscribe,
    client.getSnapshot,
  );
  return (
    <main>
      <header>
        <span className="eyebrow">SNAP / RUST CLIENT</span>
        <h1>Healthy</h1>
        <p className="description">
          A live view of the server, observed by the Rust client.
        </p>
      </header>
      <section className="monitor" aria-label="Health monitor">
        <div className="status-line">
          <span className={`indicator ${status}`} />
          <p role="status" data-status={status}>
            {status === "loading"
              ? "Checking…"
              : status === "ok"
                ? "OK"
                : "NOT OK"}
          </p>
          <span className="interval">2-second poll interval</span>
        </div>
        <div className="history" aria-label="Health check history">
          {samples.length === 0 ? (
            <span className="empty">Waiting for first sample…</span>
          ) : (
            samples.map((sample, index) => (
              <span
                key={`${sample.at}-${index}`}
                className={`sample ${sample.ok ? "ok" : "error"}`}
                title={`${sample.ok ? "OK" : "NOT OK"} · ${new Date(sample.at).toLocaleTimeString()}`}
              />
            ))
          )}
        </div>
        <footer>
          <span>LAST {samples.length} / 60 SAMPLES</span>
          <span>OLD → NEW</span>
        </footer>
      </section>
      <p className="architecture">
        React renders. Rust owns polling, status, and history.
      </p>
    </main>
  );
}
