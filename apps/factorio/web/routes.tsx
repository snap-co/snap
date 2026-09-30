import { createRoute, createRouter, redirect } from "@tanstack/react-router";
import { sessionRoot, requireSignedIn, requireSignedOut } from "../../../kits/react/router";
import { Factorio } from "../client";
import { WorkspacePage } from "./pages/workspace";
import { SignInPage } from "./pages/sign-in";

export function createAppRouter(client: Factorio) {
  const runtime = client.runtime;
  const root = sessionRoot(runtime, client);
  const signIn = createRoute({ getParentRoute: () => root, path: "/sign-in", beforeLoad: ({ context }) => requireSignedOut(context.session), component: SignInPage });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "workspace", component: Workspace, beforeLoad: ({ context, location }) => requireSignedIn(context.session, location.href), loader: async ({ context }) => {
    const epoch = context.session.epoch;
    const roots = await context.client.workspaces();
    const choices = roots.length ? [] : await context.client.repositories();
    if (epoch !== runtime.getSnapshot().epoch) throw new Error("Session changed");
    client.workspaceID = roots[0]?.id ?? "";
    return { repositories: choices };
  } });
  function Workspace() {
    const loaded = protectedRoute.useLoaderData();
    return <WorkspacePage client={client} initialRepositories={loaded.repositories} />;
  }
  const workspaceRoute = createRoute({ getParentRoute: () => protectedRoute, path: "/", beforeLoad: ({ location }) => {
    const hash = location.hash;
    if (hash.startsWith("intake-")) throw redirect({ to: "/intakes/$intakeId", params: { intakeId: hash }, replace: true });
    if (hash.startsWith("ticket-") && hash !== "ticket-editor") throw redirect({ to: "/tickets/$ticketId", params: { ticketId: hash.slice(7) }, replace: true });
    throw redirect({ to: hash === "sessions" ? "/sessions" : hash === "tickets" || hash === "ticket-editor" ? "/tickets" : "/intakes", replace: true });
  } });
  const routes = [
    createRoute({ getParentRoute: () => protectedRoute, path: "/intakes" }),
    createRoute({ getParentRoute: () => protectedRoute, path: "/intakes/$intakeId" }),
    createRoute({ getParentRoute: () => protectedRoute, path: "/tickets" }),
    createRoute({ getParentRoute: () => protectedRoute, path: "/tickets/$ticketId" }),
    createRoute({ getParentRoute: () => protectedRoute, path: "/sessions" }),
    createRoute({ getParentRoute: () => protectedRoute, path: "/sessions/$sessionId" }),
  ];
  return createRouter({ routeTree: root.addChildren([signIn, protectedRoute.addChildren([workspaceRoute, ...routes])]), context: { client, session: runtime.getSnapshot() } });
}
