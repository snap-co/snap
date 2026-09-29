import { Link } from "@tanstack/react-router";

export function LauncherPage() {
  return <section className="launcher">
    <div className="intro">
      <span className="eyebrow">SMALL APPS. REAL CONTRACTS.</span>
      <h1>Your testing ground.</h1>
      <p>Open an app. Follow a request.<br />See what the host is doing.</p>
    </div>
    <div className="app-grid">
      <Link className="app-tile" to="/healthy"><span className="app-icon health-icon">↗</span><strong>Healthy</strong><span>Check the connection</span></Link>
      <Link className="app-tile" to="/calc"><span className="app-icon calc-icon">＋<br />＝</span><strong>Calculator</strong><span>State over transport</span></Link>
    </div>
    <div className="launcher-note"><span className="status-dot" /> One host. A collection of small experiments.</div>
  </section>;
}
