// @vitest-environment jsdom
import { StrictMode, Suspense } from "react";
import { act } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { DbProvider } from "@tanstack/react-db";
import { QueryClient, QueryClientProvider, useSuspenseQuery, dehydrate, hydrate } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { orgStoreOptions } from "#/collections/org-store";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";
import { environmentChangeStateOptions } from "#/modules/deployments/environment-change-state.queries";

it("keeps the SSR canvas visible while hydrated live queries take over", async () => {
  const server = new QueryClient();
  const client = new QueryClient();
  const scope = { queryClient: server, sessionId: "session", userId: "user" };
  for (const table of orgStoreTableNames) {
    server.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
  }
  server.setQueryData(environmentChangeStateOptions("acme", scope).queryKey, []);
  await server.ensureQueryData(orgStoreOptions("acme", scope));
  const pending = vi.fn(() => <div>Loading canvas</div>);
  function Canvas({ queryClient }: { queryClient: QueryClient }) {
    useSuspenseQuery(orgStoreOptions("acme", { ...scope, queryClient }));
    return <div>Loaded canvas</div>;
  }
  function App({ queryClient }: { queryClient: QueryClient }) {
    const Pending = pending;
    return <StrictMode><DbProvider client={getDbClient(queryClient)}><QueryClientProvider client={queryClient}><Suspense fallback={<Pending />}><Canvas queryClient={queryClient} /></Suspense></QueryClientProvider></DbProvider></StrictMode>;
  }
  const html = renderToString(<App queryClient={server} />);
  const container = document.createElement("div");
  container.innerHTML = html;
  document.body.append(container);
  expect(container.textContent).toBe("Loaded canvas");
  getDbClient(client).hydrate(getDbClient(server).dehydrate({ shouldDehydrateLiveQuery: () => true }));
  hydrate(client, dehydrate(server, { shouldDehydrateQuery: query => query.queryKey[0] !== "collections" }));
  pending.mockClear();
  const errors = vi.fn();
  let root: ReturnType<typeof hydrateRoot> | undefined;
  try {
    await act(async () => { root = hydrateRoot(container, <App queryClient={client} />, { onRecoverableError: errors }); });
    expect(container.textContent).toBe("Loaded canvas");
    expect(pending).not.toHaveBeenCalled();
    expect(errors).not.toHaveBeenCalled();
  } finally {
    await act(async () => root?.unmount());
    container.remove();
    await Promise.all([getDbClient(server).cleanup(), getDbClient(client).cleanup()]);
    server.clear(); client.clear();
  }
});
