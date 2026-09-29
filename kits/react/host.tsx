/// <reference types="vite/client" />
import { createRoot } from "react-dom/client";
import { RouterProvider, type AnyRouter } from "@tanstack/react-router";
import type { SessionRuntime } from "../../platforms/browser/runtime";

/** Own subscriptions and client lifetime outside React effects, including HMR. */
export function mount<A>(options: { router: AnyRouter; runtime: SessionRuntime<A>; element: HTMLElement; dispose?: () => void }) {
  const { router, runtime } = options;
  let current = runtime.getSnapshot();
  const unsubscribe = runtime.subscribe(() => {
    const next = runtime.getSnapshot();
    if (next.epoch !== current.epoch || next.phase !== current.phase) {
      current = next;
      void router.invalidate();
    }
  });
  const revalidate = () => { if (document.visibilityState === "visible") void runtime.refresh(); };
  window.addEventListener("focus", revalidate);
  document.addEventListener("visibilitychange", revalidate);
  const root = createRoot(options.element);
  root.render(<RouterProvider router={router} />);
  return () => {
    unsubscribe();
    window.removeEventListener("focus", revalidate);
    document.removeEventListener("visibilitychange", revalidate);
    root.unmount();
    if (options.dispose) options.dispose(); else runtime.close();
  };
}
