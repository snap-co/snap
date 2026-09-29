import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { Development } from "./development";

export function Experiment({ status, title, eyebrow, busy, error, frames, children }: {
  status: string; title: string; eyebrow: string; busy: boolean; error: string; frames: string[]; children: ReactNode;
}) {
  return <>
    <nav><Link to="/">← All apps</Link><span className="pill">{status}</span></nav>
    <div className="workspace">
      <section className="application">
        <span className="eyebrow">{eyebrow}</span><h1>{title}</h1>
        {children}
        {busy && <p className="pending" role="status">Request in progress. Host controls remain available.</p>}
        {error && <p role="alert" className="error">{error}</p>}
        <details className="wire"><summary>Transport activity</summary><pre>{frames.join("\n\n")}</pre></details>
      </section>
      <Development />
    </div>
  </>;
}
