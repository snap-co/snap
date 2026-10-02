/// <reference types="vite/client" />
import { createRoot } from "react-dom/client";
import { RouterProvider, type AnyRouter } from "@tanstack/react-router";
import type { SessionRuntime } from "../browser/runtime";
import { SessionPublication } from "./publication";

/** Own subscriptions and client lifetime outside React effects, including HMR.
 * Apps with explicit per-page connections can omit the session runtime. */
export function mount<A>(options: { router: AnyRouter; runtime?: SessionRuntime<A>; element: HTMLElement; dispose?: () => void }) {
  const { router, runtime } = options;
  if (!runtime) {
    const root = createRoot(options.element);
    root.render(<RouterProvider router={router} />);
    return () => { root.unmount(); options.dispose?.(); };
  }
  let current = runtime.getSnapshot();
  let publishedEpoch: number | null = null;
  let generation = 0, invalidating = false, disposed = false;
  const listeners = new Set<() => void>();
  const publication = {
    getSnapshot: () => publishedEpoch,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
  const publish = () => {
    if (disposed || invalidating || router.state.isLoading) return;
    const epoch = runtime.getSnapshot().epoch;
    if (!router.state.matches.length || !router.state.matches.every(match => match.context.session?.epoch === epoch)) return;
    publishedEpoch = epoch;
    for (const listener of listeners) listener();
  };
  const stopResolved = router.subscribe("onResolved", publish);
  const unsubscribe = runtime.subscribe(() => {
    const next = runtime.getSnapshot();
    if (next.epoch !== current.epoch || next.phase !== current.phase) {
      current = next;
      const ticket = ++generation;
      invalidating = true;
      // Session changes retire completed loader data as well as in-flight work.
      // Ordinary background invalidation can mount the new epoch with old data.
      void router.invalidate({ sync: true, forcePending: true }).then(() => {
        if (disposed || ticket !== generation) return;
        invalidating = false;
        publish();
      });
    }
  });
  const revalidate = () => { if (document.visibilityState === "visible") void runtime.refresh(); };
  window.addEventListener("focus", revalidate);
  document.addEventListener("visibilitychange", revalidate);
  const root = createRoot(options.element);
  root.render(<SessionPublication.Provider value={publication}><RouterProvider router={router} /></SessionPublication.Provider>);
  return () => {
    disposed = true;
    unsubscribe();
    stopResolved();
    window.removeEventListener("focus", revalidate);
    document.removeEventListener("visibilitychange", revalidate);
    root.unmount();
    if (options.dispose) options.dispose(); else runtime.close();
  };
}
