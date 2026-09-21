// @vitest-environment jsdom
import { Suspense, type ReactNode } from "react";
import { act, renderHook } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { focusManager, QueryClient, QueryClientProvider, useQuery, useSuspenseQuery, dehydrate, hydrate } from "@tanstack/react-query";
import { environmentChangeStateOptions, preloadOrganizationEnvironmentChangeStateProjections, useEnvironmentProjectionVersion } from "./use-environment-state-projection";

import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { getDbClient } from "#/collections/scope";
import { getEnvironmentDeploymentsCollection, getEnvironmentSavedStateRevisionsCollection } from "#/collections/collections";
import { createApiCollection, preloadCollection } from "#/collections/query-collection";
import { serviceDeploymentKeys } from "./deployment-queries";
import type { environmentDeployment } from "./tables";

it("refreshes the Environment projection when API deployment or revision metadata changes without local writes", async () => {
  vi.useFakeTimers();
  focusManager.setFocused(true);
  const client = new QueryClient();
  let deployment: typeof environmentDeployment.$inferSelect = {
    id: "deploy", organizationId: "org", environmentId: "env", status: "deploying", updatedAt: new Date(0),
    triggerOrigin: { origin: "manual", actorId: "user" }, savedStateSnapshotId: "saved-1", serviceActionPolicy: null,
    inngestRunId: null, coreDeployId: null, retryOfDeploymentId: null, variableProducers: null,
    deployManifest: null, deployPreview: null, runtimeProgress: null, failureCode: null, failureMessage: null, message: null,
    cancellationRequestedAt: null, dispatchRequestedAt: null, startedAt: null, finishedAt: null, createdAt: new Date(0),
  };
  let revisions = [{ id: "saved-1", environmentId: "env", organizationId: "org" }];
  let marker = "initial";
  const deployments = createApiCollection({ queryClient: client, queryKey: ["projection", "deployments"],
    queryFn: async () => [deployment], getKey: (row: typeof deployment) => row.id });
  const savedStateRevisions = createApiCollection({ queryClient: client, queryKey: ["projection", "revisions"],
    queryFn: async () => revisions, getKey: (row: (typeof revisions)[number]) => row.id });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  const hook = renderHook(() => {
    const version = useEnvironmentProjectionVersion({ deployments, savedStateRevisions });
    return useQuery({ queryKey: [...serviceDeploymentKeys.environmentChangeStatesOrg("org"), version], queryFn: async () => ({ marker }) }).data;
  }, { wrapper });
  try {
    await act(async () => { await vi.advanceTimersByTimeAsync(20); });
    expect(hook.result.current).toMatchObject({ marker: "initial" });
    deployment = { ...deployment, status: "applied", updatedAt: new Date(1) };
    marker = "applied";
    await act(async () => { await vi.advanceTimersByTimeAsync(15_000); });
    await act(async () => { await vi.advanceTimersByTimeAsync(10); });
    expect(hook.result.current).toMatchObject({ marker: "applied" });
    revisions = [...revisions, { id: "saved-2", environmentId: "env", organizationId: "org" }];
    marker = "new-revision";
    await act(async () => { await vi.advanceTimersByTimeAsync(15_000); });
    await act(async () => { await vi.advanceTimersByTimeAsync(10); });
    expect(hook.result.current).toMatchObject({ marker: "new-revision" });
  } finally {
    hook.unmount();
    client.clear();
    vi.useRealTimers();
    focusManager.setFocused(undefined);
  }
});

it("hydrates the server projection without an empty-version query or a loading fallback", async () => {
  const serverClient = new QueryClient();
  const browserClient = new QueryClient();
  const serverScope = { queryClient: serverClient, sessionId: "session", userId: "user", environmentSlug: "production" };
  const browserScope = { ...serverScope, queryClient: browserClient };
  const serverMetadata = {
    deployments: getEnvironmentDeploymentsCollection("org", serverScope),
    savedStateRevisions: getEnvironmentSavedStateRevisionsCollection("org", serverScope),
  };
  serverClient.setQueryData(["collections", "session", "user", "org", "environment_deployment", "production"], []);
  serverClient.setQueryData(["collections", "session", "user", "org", "environment_saved_state_snapshot", "production"], [
    { id: "saved-1", environmentId: "env", organizationId: "org" },
  ]);
  await Promise.all(Object.values(serverMetadata).map(preloadCollection));
  const serverOptions = environmentChangeStateOptions("org", serverScope);
  serverClient.setQueryData(serverOptions.queryKey, []);
  await preloadOrganizationEnvironmentChangeStateProjections(serverScope, "org");

  const fallback = vi.fn(() => <span>Loading projection</span>);
  function Projection({ scope }: { scope: typeof serverScope }) {
    const version = useEnvironmentProjectionVersion({
      deployments: getEnvironmentDeploymentsCollection("org", scope),
      savedStateRevisions: getEnvironmentSavedStateRevisionsCollection("org", scope),
    });
    useSuspenseQuery(environmentChangeStateOptions("org", scope));
    return <span>{version}</span>;
  }
  function App({ scope }: { scope: typeof serverScope }) {
    const Fallback = fallback;
    return <QueryClientProvider client={scope.queryClient}>
      <Suspense fallback={<Fallback />}><Projection scope={scope} /></Suspense>
    </QueryClientProvider>;
  }
  const container = document.createElement("div");
  container.innerHTML = renderToString(<App scope={serverScope} />);
  expect(container.textContent).toBe("saved:saved-1");
  document.body.append(container);
  getDbClient(browserClient).hydrate(getDbClient(serverClient).dehydrate());
  hydrate(browserClient, dehydrate(serverClient, { shouldDehydrateQuery: (query) => query.queryKey[0] !== "collections" }));
  const errors: unknown[] = [];
  let root: ReturnType<typeof hydrateRoot> | undefined;
  try {
    await act(async () => { root = hydrateRoot(container, <App scope={browserScope} />, { onRecoverableError: (error) => errors.push(error) }); });
    expect(container.textContent).toBe("saved:saved-1");
    expect(fallback).not.toHaveBeenCalled();
    expect(errors).toEqual([]);
    expect(environmentChangeStateOptions("org", browserScope).queryKey).toEqual(serverOptions.queryKey);
    expect(browserClient.isFetching()).toBe(0);
  } finally {
    await act(async () => root?.unmount());
    container.remove();
    await Promise.all([getDbClient(serverClient).cleanup(), getDbClient(browserClient).cleanup()]);
    serverClient.clear();
    browserClient.clear();
  }
});
