// @vitest-environment jsdom
import { StrictMode, Suspense } from "react";
import { act } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { DbProvider } from "@tanstack/react-db";
import { QueryClient, QueryClientProvider, useSuspenseQuery, dehydrate, hydrate } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { environmentCanvasOptions } from "#/modules/environment-design/environment-data";
import { environmentChangeStateOptions, preloadOrganizationEnvironmentChangeStateProjections } from "#/modules/deployments/use-environment-state-projection";
import { getEnvironmentNodeIntroductionsCollection, getEnvironmentSavedStateRevisionsCollection } from "#/collections/collections";
import { preloadCollection } from "#/collections/query-collection";

const params = { organizationSlug: "acme", projectSlug: "app", environmentSlug: "production" };

it("keeps the SSR canvas visible while hydrated live queries take over", async () => {
  const server = new QueryClient();
  const client = new QueryClient();
  const scope = { queryClient: server, sessionId: "session", userId: "user", environmentSlug: "production" };
  for (const table of ["project", "environment", "service", "environment_resource", "resource_lineage", "environment_canvas_node_position", "environment_node_config_snapshot", "volume_remove_attempt", "environment_deployment", "environment_saved_state_snapshot", "environment_node_introduction"]) {
    server.setQueryData(["collections", "session", "user", "acme", table, ...(table === "project" ? [] : ["production"])], []);
  }
  server.setQueryData(["collections", "session", "user", "acme", "environment_saved_state_snapshot", "production"], [{ id: "saved", environmentId: "env", organizationId: "org" }]);
  await preloadCollection(getEnvironmentSavedStateRevisionsCollection("acme", scope));
  server.setQueryData(environmentChangeStateOptions("acme", scope).queryKey, []);
  await Promise.all([
    server.ensureQueryData(environmentCanvasOptions(params, scope)),
    preloadOrganizationEnvironmentChangeStateProjections(scope, "acme"),
    preloadCollection(getEnvironmentNodeIntroductionsCollection("acme", scope)),
  ]);
  const pending = vi.fn(() => <div>Loading canvas</div>);
  function Canvas({ queryClient }: { queryClient: QueryClient }) {
    useSuspenseQuery(environmentCanvasOptions(params, { ...scope, queryClient }));
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
