import { Link, Outlet } from "@tanstack/react-router";

export function Layout() {
  return <main>
    <header>
      <Link className="brand" to="/">s<span>snap</span></Link>
      <span className="eyebrow">LOCAL PLAYGROUND</span>
      <span className="version">TESTY / 01</span>
    </header>
    <Outlet />
    <footer><span>SNAP / TESTY</span><span>Host-owned state. Observable execution.</span></footer>
  </main>;
}
