// @vitest-environment jsdom
import { act } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeSnapshot } from "#/modules/runtime/runtime.collection";
import { useServerList } from "./use-servers";

const queryClient = new QueryClient();
const scope = { queryClient, sessionId: "session", userId: "user" };

const observed: RuntimeSnapshot = {
  status: "observed",
  error: null,
  machines: [
    {
      id: "m1",
      name: "fra-1",
      publicIp: null,
      acceptsIngress: true,
      endpoints: [],
      acceptsBuilds: true,
      acceptsServices: true,
      storage: null,
      buildConcurrency: null,
      effectiveBuildConcurrency: 1,
      runningBuilds: 0,
      daemonVersion: "0.2.1",
      capabilities: [],
      membership: "up",
      observedContainerCount: 0,
      observedAt: "2026-07-01T00:00:00.000Z",
    },
  ],
  services: [],
  volumeCopies: [],
  certificates: [],
  published: [],
  incompleteIds: { machines: [], containers: [], volumes: [], certificates: [] },
  observedAt: "2026-07-01T00:00:00.000Z",
};

async function serverPage() {
  const root = createRootRoute();
  const protectedRoute = createRoute({
    getParentRoute: () => root,
    id: "_protected",
    beforeLoad: () => ({ session: { session: { id: scope.sessionId }, user: { id: scope.userId } } }),
  });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([index])]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  return (
    <QueryClientProvider client={queryClient}>
      <RouterContextProvider router={router}>
        <ServerList />
      </RouterContextProvider>
    </QueryClientProvider>
  );
}

function ServerList() {
  const { state, servers } = useServerList("acme");
  return <p>{state === "loading" ? "Server loading" : servers.map((server) => server.name).join(",")}</p>;
}

afterEach(async () => {
  document.body.innerHTML = "";
  await getDbClient(queryClient).cleanup();
  queryClient.clear();
});

it("hydrates the server's loading state when a Runtime Watch frame lands before hydration", async () => {
  const page = await serverPage();
  const container = document.createElement("div");
  container.innerHTML = renderToString(page);
  document.body.append(container);
  expect(container.textContent).toBe("Server loading");

  applyRuntimeSnapshot({ collections: getRuntimeCollections("acme", scope), snapshot: observed });
  const errors = vi.fn();
  let root: ReturnType<typeof hydrateRoot> | undefined;
  try {
    await act(async () => {
      root = hydrateRoot(container, page, { onRecoverableError: errors });
    });
    expect(errors).not.toHaveBeenCalled();
    expect(container.textContent).toBe("fra-1");
  } finally {
    await act(async () => root?.unmount());
  }
});
