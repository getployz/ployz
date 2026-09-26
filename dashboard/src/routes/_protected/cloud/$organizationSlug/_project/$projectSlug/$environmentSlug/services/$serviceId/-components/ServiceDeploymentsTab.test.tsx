// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import { Tabs } from "#/components/ui/tabs";
import { nodeDeploymentsQueryOptions, type NodeDeployment } from "#/modules/deployments/deployment-history.queries";
import { canvasRouteSearch } from "../../../-components/deployment-mode";
import { ServiceDeploymentsTab } from "./ServiceDeploymentsTab";

const environmentId = "00000000-0000-4000-8000-000000000003";
const [first, third] = ["00000000-0000-4000-8000-000000000011", "00000000-0000-4000-8000-000000000013"];
const api = "00000000-0000-4000-8000-000000000021";
const createdAt = new Date("2026-09-01T00:00:00Z");
const attempt = (id: string, message: string, outcome: NodeDeployment["outcome"]): NodeDeployment => ({ id, message, createdAt, outcome });

async function openTab() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  // The server read's one page: `first` still serves api after `third` failed it; History holds `first` too.
  queryClient.setQueryData(nodeDeploymentsQueryOptions("acme", environmentId, api).queryKey, {
    pages: [{ running: attempt(first, "Ship api", "deployed"), items: [attempt(third, "Bump api", "failed"), attempt(first, "Ship api", "deployed")], next: null }],
    pageParams: [undefined],
  });

  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", loader: () => ({ environmentId }), component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas", ...canvasRouteSearch, component: Outlet });
  const serviceRoute = createRoute({ getParentRoute: () => canvas, path: "services/$serviceId", component: () => (
    <Tabs value="deployments"><ServiceDeploymentsTab organizationSlug="acme" serviceId={api} /></Tabs>
  ) });
  const routeTree = root.addChildren([protectedRoute.addChildren([organization.addChildren([
    projectGroup.addChildren([environment.addChildren([canvas.addChildren([serviceRoute])])]),
  ])])]);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: [`/cloud/acme/shop/production/services/${api}`] }) });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByText("History");
  return router;
}

afterEach(cleanup);

it("shows the deployment still serving the service, its changing history, and opens either in Deployment Mode", async () => {
  const router = await openTab();
  const running = screen.getByText("Running").closest("a");
  expect(running?.textContent).toContain("Ship api");
  // The Running attempt shows once.
  expect(screen.getAllByText(/Ship api/)).toHaveLength(1);
  expect(running?.getAttribute("href")).toBe(`/cloud/acme/shop/production/services/${api}?deployment=${first}&tab=deploy-logs`);
  expect(screen.getByText(/Bump api/).closest("a")?.textContent).toContain("Failed");
  expect(screen.queryByText("Show more")).toBeNull();

  const row = screen.getByText(/Bump api/).closest("a");
  if (!row) throw new Error("Missing history row");
  await act(async () => { fireEvent.click(row); });
  expect(router.state.location.pathname).toBe(`/cloud/acme/shop/production/services/${api}`);
  expect(router.state.location.search).toEqual({ deployment: third });
});
