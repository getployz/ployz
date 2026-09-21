// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { DbProvider, collectionOptions, liveQueryCollectionOptions, useLiveQuery } from "@tanstack/react-db";
import { QueryClient } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { createApiCollection, preloadCollection } from "./query-collection";
import { getDbClient } from "./scope";

afterEach(cleanup);

function rows(queryClient: QueryClient, read: () => Promise<{ id: string; name: string }[]>) {
  return createApiCollection({
    queryClient, queryKey: ["ssr", "session", "organization"], queryFn: read,
    getKey: (row: { id: string; name: string }) => row.id,
  });
}

function Names({ collection }: { collection: ReturnType<typeof rows> }) {
  const { data = [] } = useLiveQuery({ query: (q) => q.from({ row: collection }) });
  return <ul>{data.map((row) => <li key={row.id}>{row.name}</li>)}</ul>;
}

it("hydrates the server result before live reads and committed writes take over", async () => {
  const serverQuery = new QueryClient();
  const serverDb = getDbClient(serverQuery);
  const serverRows = rows(serverQuery, async () => [{ id: "one", name: "Server snapshot" }]);
  await preloadCollection(serverRows);
  await serverDb.preloadLiveQuery({ query: (q) => q.from({ row: serverRows }) });
  const markup = renderToString(<DbProvider client={serverDb}><Names collection={serverRows} /></DbProvider>);
  expect(markup).toContain("Server snapshot");
  const state = serverDb.dehydrate();

  const browserQuery = new QueryClient();
  const browserDb = getDbClient(browserQuery);
  browserDb.hydrate(state);
  const browserRows = rows(browserQuery, async () => [{ id: "one", name: "Live snapshot" }]);
  const container = document.createElement("div");
  container.innerHTML = markup;
  document.body.append(container);
  const onRecoverableError = vi.fn();
  const view = render(<DbProvider client={browserDb}><Names collection={browserRows} /></DbProvider>, {
    container, hydrate: true, onRecoverableError,
  });
  await screen.findByText("Live snapshot");
  expect(onRecoverableError).not.toHaveBeenCalled();
  await browserRows.writeCommitted({ id: "one", name: "Committed edit" });
  await waitFor(() => expect(screen.getByText("Committed edit")).toBeTruthy());
  expect(serverRows.get("one")?.name).toBe("Server snapshot");
  view.unmount();
  await Promise.all([serverDb.cleanup(), browserDb.cleanup()]);
  serverQuery.clear(); browserQuery.clear();
});

it("isolates collections with the same identity between server requests", async () => {
  const firstQuery = new QueryClient();
  const secondQuery = new QueryClient();
  const first = rows(firstQuery, async () => [{ id: "one", name: "First request" }]);
  const second = rows(secondQuery, async () => [{ id: "one", name: "Second request" }]);
  await Promise.all([preloadCollection(first), preloadCollection(second)]);
  expect(first).not.toBe(second);
  expect(first.get("one")?.name).toBe("First request");
  expect(second.get("one")?.name).toBe("Second request");
  await Promise.all([getDbClient(firstQuery).cleanup(), getDbClient(secondQuery).cleanup()]);
  firstQuery.clear(); secondQuery.clear();
});

it("releases dependent queries before their sources when a server request finishes", async () => {
  const queryClient = new QueryClient();
  const client = getDbClient(queryClient);
  const source = rows(queryClient, async () => [{ id: "one", name: "Snapshot" }]);
  const first = client.collection(collectionOptions(liveQueryCollectionOptions({
    id: "first", query: (q) => q.from({ row: source }),
  })));
  const second = client.collection(collectionOptions(liveQueryCollectionOptions({
    id: "second", query: (q) => q.from({ row: first }),
  })));
  await client.preloadLiveQuery({ query: (q) => q.from({ row: second }) });
  const errors = vi.spyOn(console, "error");
  try {
    await client.cleanup();
    expect(errors).not.toHaveBeenCalled();
    expect([source.status, first.status, second.status]).toEqual([
      "cleaned-up", "cleaned-up", "cleaned-up",
    ]);
  } finally {
    errors.mockRestore();
    queryClient.clear();
  }
});
