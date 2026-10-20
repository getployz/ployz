// @vitest-environment jsdom
import { act, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import { getDbClient } from "#/collections/scope";
import {
  applyRuntimeSnapshot, getRuntimeCollections, unreachableRuntimeSnapshot, type RuntimeSnapshot,
} from "#/modules/runtime/runtime.collection";
import { useServerList } from "./use-servers";

const queryClient = new QueryClient();
const scope = { queryClient, sessionId: "session", userId: "user" };

const machine = (id: string, name: string, membership: "up" | "down") => ({
  id, name, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: true, acceptsServices: true, storage: null,
  buildConcurrency: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.2.1", capabilities: [], membership,
  observedContainerCount: 0, observedAt: "2026-07-01T00:00:00.000Z",
});

const mixed: RuntimeSnapshot = {
  status: "observed",
  error: null,
  machines: [machine("m1", "fra-1", "up"), machine("m2", "fra-2", "down")],
  services: [],
  volumeCopies: [],
  certificates: [],
  published: [],
  incompleteIds: { machines: [], containers: [], volumes: [], certificates: [] },
  observedAt: "2026-07-01T00:00:00.000Z",
};

function ServerList({ organizationSlug }: { organizationSlug: string }) {
  const { servers, unreachable } = useServerList(organizationSlug);
  return <p>{unreachable ? "unreachable" : servers.map((server) => `${server.name} ${server.status}`).join(", ")}</p>;
}

async function show(organizationSlug: string, snapshot: RuntimeSnapshot) {
  const root = createRootRoute();
  const protectedRoute = createRoute({
    getParentRoute: () => root,
    id: "_protected",
    beforeLoad: () => ({ session: { session: { id: scope.sessionId }, user: { id: scope.userId } } }),
  });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([createRoute({ getParentRoute: () => protectedRoute, path: "/" })])]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  applyRuntimeSnapshot({ collections: getRuntimeCollections(organizationSlug, scope), snapshot });
  await act(async () => {
    render(<QueryClientProvider client={queryClient}><RouterContextProvider router={router}><ServerList organizationSlug={organizationSlug} /></RouterContextProvider></QueryClientProvider>);
  });
  return screen.getByRole("paragraph").textContent;
}

afterEach(async () => {
  document.body.innerHTML = "";
  await getDbClient(queryClient).cleanup();
  queryClient.clear();
});

it("reads unreachable when Cloud reached none of the Servers", async () => {
  expect(await show("acme", unreachableRuntimeSnapshot("The cluster is expected but unreachable."))).toBe("unreachable");
});

it("reads reachable while one Server answers and another is offline", async () => {
  expect(await show("globex", mixed)).toBe("fra-2 offline, fra-1 online");
});
