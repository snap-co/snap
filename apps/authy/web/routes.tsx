import { createRoute, createRouter, redirect } from "@tanstack/react-router";
import { sessionRoot, requireSignedIn, requireSignedOut } from "../../../kits/react/router";
import { AuthyClient } from "../client";
import { AccountPage } from "./pages/account";
import { SignInPage } from "./pages/sign-in";

export function createAppRouter(client: AuthyClient) {
  const root = sessionRoot(client.runtime, client);
  const index = createRoute({ getParentRoute: () => root, path: "/", beforeLoad: ({ context }) => { throw redirect({ to: context.session.account ? "/account" : "/sign-in", search: true, replace: true }); } });
  const signIn = createRoute({ getParentRoute: () => root, path: "/sign-in", beforeLoad: ({ context }) => requireSignedOut(context.session, "/account"), component: () => <SignInPage client={client} /> });
  const account = createRoute({ getParentRoute: () => root, path: "/account", beforeLoad: ({ context, location }) => requireSignedIn(context.session, location.href), component: () => <AccountPage client={client} /> });
  return createRouter({ routeTree: root.addChildren([index, signIn, account]), context: { client, session: client.runtime.getSnapshot() } });
}
