import { createRoute, createRouter, redirect } from "@tanstack/react-router";
import { sessionRoot, requireSignedIn, requireSignedOut } from "../../../kits/react/router";
import { Factorio } from "../client";
import { WorkspacePage } from "./pages/workspace";
import { SignInPage } from "./pages/sign-in";

export function createAppRouter(client: Factorio) {
  const runtime = client.runtime!;
  const root = sessionRoot(runtime, client);
  const signIn = createRoute({ getParentRoute: () => root, path: "/sign-in", beforeLoad: ({ context }) => requireSignedOut(context.session), component: SignInPage });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "workspace", beforeLoad: ({ context, location }) => requireSignedIn(context.session, location.href), loader: async ({ context }) => {
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
    if (location.hash.startsWith("intake-")) throw redirect({ to: "/intakes/$intakeId", params: { intakeId: location.hash }, replace: true });
  }, component: Workspace });
  const intakeRoute = createRoute({ getParentRoute: () => protectedRoute, path: "/intakes/$intakeId", component: Workspace });
  return createRouter({ routeTree: root.addChildren([signIn, protectedRoute.addChildren([workspaceRoute, intakeRoute])]), context: { client, session: runtime.getSnapshot() } });
}
