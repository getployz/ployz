// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { compileSavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { beforeEach, expect, it, vi } from "vitest";
import { useCanvasChangeActions } from "./useCanvasChangeActions";

import { createRouter, createRootRoute, createMemoryHistory, RouterContextProvider } from "@tanstack/react-router";
import { toast } from "sonner";
import * as commands from "#/modules/deployments/deployment.functions";
import * as deploymentCollections from "#/modules/deployments/deployment-collection";
import * as scopes from "#/collections/use-collection-scope";
import * as collections from "#/collections/collections";
import * as documents from "#/modules/environment-design/environment-document.collection";
import * as runtime from "#/modules/runtime/use-runtime-lens";
import * as preflight from "#/modules/runtime/deploy-target-preflight";
import { asTestDouble } from "#/lib/test-double";

const mocks = {
  submit: vi.spyOn(commands, "submitReviewedPublicationServerFn"),
  prepare: vi.spyOn(commands, "prepareEnvironmentDestructiveVolumesServerFn"),
  reconcile: vi.spyOn(deploymentCollections, "reconcileDeploymentCollections").mockResolvedValue(undefined),
  toast: vi.spyOn(toast, "error").mockReturnValue("toast"), open: vi.fn(), clearMessage: vi.fn(),
};
vi.spyOn(scopes, "useCollectionScope").mockReturnValue(asTestDouble<ReturnType<typeof scopes.useCollectionScope>>()({}));
vi.spyOn(collections, "getEnvironmentsCollection").mockReturnValue(asTestDouble<ReturnType<typeof collections.getEnvironmentsCollection>>()({}));
vi.spyOn(documents, "useEnvironmentDocument").mockImplementation(() => asTestDouble<NonNullable<ReturnType<typeof documents.useEnvironmentDocument>>>()({
  id: "env", revision: "reviewed-revision", compiled: compileSavedEnvironmentIntent({ environmentId: "env", intent: {
    version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [],
  } }),
}));
vi.spyOn(runtime, "useRuntimeLens").mockReturnValue(asTestDouble<ReturnType<typeof runtime.useRuntimeLens>>()({ status: "ready", machines: [{}], isLoading: false }));
vi.spyOn(preflight, "getDeployTargetPreflight").mockReturnValue({ ok: true });

beforeEach(() => {
  vi.clearAllMocks();
  mocks.prepare.mockResolvedValue([]);
  mocks.submit.mockImplementation(async ({ data }) => ({ state: data.intent === "manual_deploy" ? "deployment_queued" : "saved" }));
});

function renderActions(destructiveServiceIds = ["removed-service"]) {
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  const router = createRouter({ routeTree: createRootRoute(), history: createMemoryHistory({ initialEntries: ["/"] }) });
  const hook = renderHook(({ savedId, destructiveServiceIds }) => useCanvasChangeActions({
    environmentId: "env", params: { organizationSlug: "org", projectSlug: "project", environmentSlug: "production" },
    changeState: { headToken: "applied:none", groups: [], totalCount: 0 },
    savedSnapshotSource: { kind: "saved", environmentSavedStateSnapshotId: savedId },
    destructiveServiceIds, deletedDeployedVolumeIds: [], commitMessage: "Reviewed",
    setCommitMessage: mocks.clearMessage, setDestructiveConfirmationOpen: mocks.open,
  }), { initialProps: { savedId: "reviewed-saved", destructiveServiceIds }, wrapper: ({ children }) => <QueryClientProvider client={queryClient}><RouterContextProvider router={router}>{children}</RouterContextProvider></QueryClientProvider> });
  return { ...hook, queryClient };
}

it.each([false, true])("submits the captured review after live Saved changes and never resubmits a conflict (deploy=%s)", async (deploy) => {
  const { result, rerender, unmount, queryClient } = renderActions();
  try {
    await act(() => deploy ? result.current.requestDeploy() : result.current.requestSave());
    expect(mocks.open).toHaveBeenCalledWith(true);
    expect(mocks.submit).not.toHaveBeenCalled();
    const review = await result.current.prepareDestructiveReview();
    rerender({ savedId: "newer-saved", destructiveServiceIds: ["removed-service"] });
    await act(() => result.current.confirmDestructiveAction(review));
    expect(mocks.submit).toHaveBeenCalledWith({ data: expect.objectContaining({
      intent: deploy ? "manual_deploy" : "save",
      review: {
        destructiveServiceIds: ["removed-service"], destructiveVolumeReviews: [],
        savedStateBasis: { kind: "saved_revision", savedStateSnapshotId: "reviewed-saved" },
        workingStateFingerprint: expect.stringMatching(/^environment-working-state-v1:/),
      },
    }) });
    expect(mocks.reconcile).toHaveBeenCalledTimes(1);
    mocks.submit.mockRejectedValueOnce(new Error("Working State changed after publication was reviewed."));
    await act(async () => {
      await expect(result.current.confirmDestructiveAction(review)).rejects.toThrow("Working State changed");
    });
    expect(mocks.submit).toHaveBeenCalledTimes(2);
    expect(mocks.reconcile).toHaveBeenCalledTimes(1);
  } finally {
    unmount();
    queryClient.clear();
  }
});

it.each([false, true])("reconciles a committed dispatch failure and reports it without resubmitting (destructive=%s)", async (destructive) => {
  mocks.submit.mockResolvedValueOnce({ state: "attempt_dispatch_failed" });
  const { result, unmount, queryClient } = renderActions(destructive ? ["removed-service"] : []);
  try {
    await act(() => result.current.requestDeploy());
    if (destructive) {
      const review = await result.current.prepareDestructiveReview();
      await act(async () => {
        expect(await result.current.confirmDestructiveAction(review)).toEqual({ state: "submitted" });
      });
    }
    expect(mocks.reconcile).toHaveBeenCalledTimes(1);
    expect(mocks.toast).toHaveBeenCalledWith("Changes saved, but deployment could not start. Review the failed deployment before retrying.");
    expect(mocks.clearMessage).toHaveBeenCalledWith("");
    expect(mocks.submit).toHaveBeenCalledTimes(1);
  } finally {
    unmount();
    queryClient.clear();
  }
});
