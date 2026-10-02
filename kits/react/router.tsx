import { createRootRouteWithContext, Outlet, redirect, useRouter } from "@tanstack/react-router";
import { useContext, useSyncExternalStore } from "react";
import type { SessionRuntime, SessionState } from "../browser/runtime";
import { Failure, Loading } from "./states";
import { SessionPublication } from "./publication";
import "./theme.css";

export interface SessionContext<A, C> { session: SessionState<A>; client: C }

/** Apps add ordinary TanStack routes. Only this layout resolves bootstrap. */
export function sessionRoot<A, C>(runtime: SessionRuntime<A>, client: C) {
  function Shell() {
    const state = useSyncExternalStore(runtime.subscribe, runtime.getSnapshot);
    const publication = useContext(SessionPublication);
    const publishedEpoch = useSyncExternalStore(publication.subscribe, publication.getSnapshot);
    // Clear mounted identity-owned page state before an async router invalidation
    // can publish the next session. Physical reconnects keep the same epoch.
    if (state.phase === "loading") return <Loading />;
    if (state.phase === "error") return <ErrorScreen />;
    if (publishedEpoch !== state.epoch) return <Loading />;
    return <><Outlet key={state.epoch} />{state.phase === "ready" && state.connection !== "connected" && <p className="snap-reconnecting" role="status">Reconnecting…</p>}</>;
  }
  function ErrorScreen() {
    const router = useRouter();
    return <Failure retry={() => { void runtime.refresh().then(() => router.invalidate()); }} />;
  }
  return createRootRouteWithContext<SessionContext<A, C>>()({
    beforeLoad: async ({ location }) => {
      const session = await runtime.resolve();
      if (session.account && (location.pathname === "/" || location.pathname === "/sign-in")) {
        const destination = sessionStorage.getItem("snap:return-to");
        sessionStorage.removeItem("snap:return-to");
        if (destination && destination !== location.href && destination.startsWith("/") && !destination.startsWith("//") && !/[\\\r\n]/.test(destination)) throw redirect({ href: destination, reloadDocument: false, replace: true });
      }
      return { session, client };
    },
    component: Shell,
    pendingComponent: Loading,
    errorComponent: ErrorScreen,
    notFoundComponent: () => <main className="snap-state"><h1>Page not found</h1><a href="/">Return home</a></main>,
    pendingMs: 0,
    pendingMinMs: 0,
  });
}
export function requireSignedIn<A>(session: SessionState<A>, destination?: string, to = "/sign-in") {
  if (!session.account) {
    if (destination && destination !== "/") sessionStorage.setItem("snap:return-to", destination);
    throw redirect({ to });
  }
}
export function requireSignedOut<A>(session: SessionState<A>, to = "/") {
  if (session.account) throw redirect({ to });
}
