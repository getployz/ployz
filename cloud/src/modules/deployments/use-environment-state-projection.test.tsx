// @vitest-environment jsdom
import { type ReactNode } from "react";
import { act, renderHook } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { focusManager, QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { useEnvironmentProjectionRefresh } from "./use-environment-state-projection";

import { createApiCollection } from "#/collections/query-collection";
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
    deployManifest: null, deployPreview: null, failureCode: null, failureMessage: null, message: null,
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
    useEnvironmentProjectionRefresh({ organizationSlug: "org", environmentId: "env" }, { deployments, savedStateRevisions });
    return useQuery({ queryKey: serviceDeploymentKeys.environmentChangeStatesOrg("org"), queryFn: async () => ({ marker }) }).data;
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
