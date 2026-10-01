import { createRoute, createRouter } from "@tanstack/react-router";
import { sessionRoot, requireSignedIn, requireSignedOut } from "../../../kits/react/router";
import { Chatty } from "./client";
import { ConversationsPage } from "./pages/conversations";
import { SignInPage } from "./pages/sign-in";

export function createAppRouter(client: Chatty) {
  const root = sessionRoot(client.runtime, client);
  const signIn = createRoute({ getParentRoute: () => root, path: "/sign-in", beforeLoad: ({ context }) => requireSignedOut(context.session), component: SignInPage });
  const conversations = createRoute({ getParentRoute: () => root, path: "/", validateSearch: (search: Record<string, unknown>): { thread?: string } => ({ thread: typeof search.thread === "string" ? search.thread : undefined }), beforeLoad: ({ context, location }) => requireSignedIn(context.session, location.href), component: () => <ConversationsPage sdk={client} /> });
  return createRouter({ routeTree: root.addChildren([signIn, conversations]), context: { client, session: client.runtime.getSnapshot() } });
}
