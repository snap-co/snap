import { useEffect, useState } from "react";
import { createRoute, createRouter } from "@tanstack/react-router";
import type { SessionRuntime, SessionState } from "../../../crates/platform/wasm-browser/runtime";
import { sessionRoot, requireSignedIn } from "../router";
import { mount } from "../host";

let state: SessionState<{ id: string }> = { phase: "ready", account: { id: "A" }, epoch: 1, connection: "connected", error: null };
const listeners = new Set<() => void>();
const waiters: (() => void)[] = [];
let release!: () => void;
const held = new Promise<void>(resolve => { release = resolve; });
const runtime: SessionRuntime<{ id: string }> = {
  getSnapshot: () => state,
  subscribe: listener => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  resolve: async () => { if (state.phase === "loading") await new Promise<void>(r => waiters.push(r)); return state; },
  refresh: async () => state,
  close() {},
};
const root = sessionRoot(runtime, {});
const workspace = createRoute({
  getParentRoute: () => root, path: "/",
  beforeLoad: ({ context }) => requireSignedIn(context.session),
  loader: async ({ context }) => {
    const owner = context.session.account!.id;
    if (owner === "B") {
      document.documentElement.dataset.loader = "held";
      await held;
    }
    return { owner, repositories: owner === "B" ? ["repository"] : [] };
  },
  component: function Workspace() {
    const data = workspace.useLoaderData();
    const [repositories, setRepositories] = useState<string[]>([]);
    useEffect(() => { setRepositories(data.repositories); }, []);
    return <main><p>Owner {data.owner}</p><select aria-label="Repository">{repositories.map(id => <option key={id}>{id}</option>)}</select></main>;
  },
});
const router = createRouter({ routeTree: root.addChildren([workspace]), context: { client: {}, session: state } });
mount({ router, runtime, element: document.getElementById("root")! });
Object.assign(window, {
  switchStart() { state = { ...state, account: { id: "B" }, phase: "loading", epoch: 2 }; listeners.forEach(fn => fn()); },
  switchReady() { state = { ...state, phase: "ready" }; waiters.splice(0).forEach(fn => fn()); listeners.forEach(fn => fn()); },
  release,
  disconnect() { state = { ...state, connection: "disconnected" }; listeners.forEach(fn => fn()); },
});
