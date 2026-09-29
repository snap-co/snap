export function Loading() {
  return <main className="snap-state" aria-busy="true"><h1>Opening your workspace</h1><p role="status">Checking your session and synchronizing documents…</p></main>;
}
export function Failure({ retry }: { retry: () => void }) {
  return <main className="snap-state"><h1>Unable to open your workspace</h1><p role="alert">Your session or connection could not be loaded. Check your connection and try again.</p><button onClick={retry}>Try again</button></main>;
}
