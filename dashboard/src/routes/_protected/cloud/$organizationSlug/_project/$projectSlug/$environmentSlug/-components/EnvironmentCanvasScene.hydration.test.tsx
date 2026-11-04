// @vitest-environment jsdom
import { StrictMode, Suspense } from "react";
import { act } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { DbProvider, eq, useLiveSuspenseQuery, type InitialQueryBuilder } from "@tanstack/react-db";
import { QueryClient, QueryClientProvider, useSuspenseQuery, dehydrate, hydrate } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { orgStoreOptions } from "#/collections/org-store";
import { createChangeCollection } from "#/collections/query-collection";
import type { CollectionRead } from "#/collections/read.contract";
import { getDbClient, type CollectionScope } from "#/collections/scope";
import { canvasPositionKey, type CanvasPosition } from "#/modules/canvas/canvas-positions";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";

const environmentId = "6f1c3f1e-9c0e-4d2a-8a53-7d8a0b7e4c21";
const position: CanvasPosition = {
  id: "a1a0d7c4-2a4b-4c1e-9d7f-3e2b1c0a9f58", organizationId: "c2f0b1e9-7d3a-4b6c-8e5f-1a2b3c4d5e6f", environmentId,
  resourceType: "service", resourceId: "d3e4f5a6-b7c8-4d9e-8f0a-1b2c3d4e5f60", x: 10, y: 20, createdAt: new Date(0), updatedAt: new Date(0),
};
type Positions = ReturnType<typeof positionsCollection>;

function positionsCollection(scope: CollectionScope, read: (since: string | undefined) => Promise<CollectionRead<CanvasPosition>>) {
  return createChangeCollection<CanvasPosition>({
    queryClient: scope.queryClient,
    queryKey: ["collections", scope.sessionId, scope.userId, "acme", "canvas_positions"],
    getKey: canvasPositionKey,
    read: ({ since }) => read(since),
  });
}

function positionsQuery(positions: Positions) {
  return {
    queryKey: ["canvas-positions", positions.id, environmentId],
    query: (q: InitialQueryBuilder) => q.from({ position: positions }).where(({ position }) => eq(position.environmentId, environmentId)).select(({ position }) => position),
  };
}

it("hydrates the SSR canvas without a fallback and resumes change reads from the server cursor", async () => {
  const server = new QueryClient();
  const client = new QueryClient();
  const serverScope = { queryClient: server, sessionId: "session", userId: "user" };
  const clientScope = { ...serverScope, queryClient: client };
  for (const table of orgStoreTableNames) server.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
  server.setQueryData(["collections", "session", "user", "acme", "canvas_positions"], { rows: [position], cursor: "acme.7" });
  await server.ensureQueryData(orgStoreOptions("acme", serverScope));
  const read = vi.fn<(since: string | undefined) => Promise<CollectionRead<CanvasPosition>>>(async () => ({ full: false, rows: [], deleted: [], cursor: "acme.8" }));
  const serverPositions = positionsCollection(serverScope, read);
  const clientPositions = positionsCollection(clientScope, read);
  await getDbClient(server).preloadLiveQuery(positionsQuery(serverPositions));
  const pending = vi.fn(() => <div>Loading canvas</div>);
  function Canvas({ scope, positions }: { scope: CollectionScope; positions: Positions }) {
    useSuspenseQuery(orgStoreOptions("acme", scope));
    const { data } = useLiveSuspenseQuery(positionsQuery(positions));
    return <div>Loaded canvas: {data.map((row) => row.resourceId).join(",")}</div>;
  }
  function App({ scope, positions }: { scope: CollectionScope; positions: Positions }) {
    const Pending = pending;
    return <StrictMode><DbProvider client={getDbClient(scope.queryClient)}><QueryClientProvider client={scope.queryClient}><Suspense fallback={<Pending />}><Canvas scope={scope} positions={positions} /></Suspense></QueryClientProvider></DbProvider></StrictMode>;
  }
  const html = renderToString(<App scope={serverScope} positions={serverPositions} />);
  const container = document.createElement("div");
  container.innerHTML = html;
  document.body.append(container);
  expect(container.textContent).toBe(`Loaded canvas: ${position.resourceId}`);
  expect(read).not.toHaveBeenCalled();
  getDbClient(client).hydrate(getDbClient(server).dehydrate());
  hydrate(client, dehydrate(server));
  pending.mockClear();
  const errors = vi.fn();
  let root: ReturnType<typeof hydrateRoot> | undefined;
  try {
    await act(async () => { root = hydrateRoot(container, <App scope={clientScope} positions={clientPositions} />, { onRecoverableError: errors }); });
    expect(pending).not.toHaveBeenCalled();
    expect(errors).not.toHaveBeenCalled();
    expect(container.textContent).toBe(`Loaded canvas: ${position.resourceId}`);
    expect(read).not.toHaveBeenCalled();
    await act(() => clientPositions.utils.refetch());
    expect(read.mock.calls).toEqual([["acme.7"]]);
  } finally {
    await act(async () => root?.unmount());
    container.remove();
    await Promise.all([getDbClient(server).cleanup(), getDbClient(client).cleanup()]);
    server.clear(); client.clear();
  }
});
