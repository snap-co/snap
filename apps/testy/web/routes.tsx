import { createRootRouteWithContext, createRoute, createRouter, Link } from "@tanstack/react-router";
import { TestyClient } from "./client";
import { Layout } from "./pages/layout";
import { LauncherPage } from "./pages/launcher";
import { CalculatorPage } from "./pages/calculator";
import { HealthyPage } from "./pages/healthy";

export function createAppRouter(client: TestyClient) {
  const root = createRootRouteWithContext<{ client: TestyClient }>()({
    beforeLoad: async () => { await client.ready; },
    component: Layout,
    errorComponent: () => <main><h1>Unable to open Testy</h1><button onClick={() => location.reload()}>Try again</button></main>,
    notFoundComponent: () => <main><h1>Page not found</h1><Link to="/">Return home</Link></main>,
  });
  const launcher = createRoute({ getParentRoute: () => root, path: "/", component: LauncherPage });
  const calculator = createRoute({ getParentRoute: () => root, path: "/calc", component: () => <CalculatorPage client={client} /> });
  const healthy = createRoute({ getParentRoute: () => root, path: "/healthy", component: () => <HealthyPage client={client} /> });
  return createRouter({ routeTree: root.addChildren([launcher, calculator, healthy]), context: { client } });
}
