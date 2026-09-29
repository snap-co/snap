import { createRoute, createRouter } from "@tanstack/react-router";
import { mount } from "../../../kits/react/host";
import { sessionRoot, requireSignedIn, requireSignedOut } from "../../../kits/react/router";
import { App } from "./pages";
import { Chatty } from "../client";

const sdk = new Chatty();
const root = sessionRoot(sdk.runtime, sdk);
const signIn = createRoute({ getParentRoute: () => root, path: "/sign-in", beforeLoad: ({ context }) => requireSignedOut(context.session), component: () => <App sdk={sdk} /> });
const conversations = createRoute({ getParentRoute: () => root, path: "/", validateSearch: (search: Record<string, unknown>): { thread?: string } => ({ thread: typeof search.thread === "string" ? search.thread : undefined }), beforeLoad: ({ context, location }) => requireSignedIn(context.session, location.href), component: () => <App sdk={sdk} /> });
const router = createRouter({ routeTree: root.addChildren([signIn, conversations]), context: { client: sdk, session: sdk.runtime.getSnapshot() } });
const dispose = mount({ router, runtime: sdk.runtime, element: document.getElementById("root")!, dispose: () => sdk.close() });
if (import.meta.hot) import.meta.hot.dispose(dispose);
