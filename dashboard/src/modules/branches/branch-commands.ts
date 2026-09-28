import { useMutation } from "@tanstack/react-query";
import { toast } from "sonner";
import { useNavigate } from "@tanstack/react-router";
import {
  getBranchesCollection,
  getCanvasPositionsCollection,
  getEnvironmentDeploymentsCollection,
  getEnvironmentNodeIntroductionsCollection,
  getEnvironmentsCollection,
  getEnvironmentSummariesCollection,
  getRawEnvironmentResourcesCollection,
  getRawServicesCollection,
} from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { refetchEnvironmentChangeStates } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocumentQueue } from "#/modules/environment-design/environment-document-edit";
import { createBranchServerFn, makeOwnCopyServerFn, mergeBranchServerFn, updateBranchServerFn } from "./branch-functions";
import type { CreateBranch, MergeBranch } from "./branch-schemas";

/** Every Org Store table creating, merging or updating a Branch writes rows into. */
const branchTables = [
  getEnvironmentsCollection, getEnvironmentSummariesCollection, getBranchesCollection, getRawServicesCollection,
  getRawEnvironmentResourcesCollection, getCanvasPositionsCollection, getEnvironmentNodeIntroductionsCollection,
  getEnvironmentDeploymentsCollection,
];

/**
 * Creates a Branch and its first deployment, then opens its canvas. Awaited: the server assigns the Branch's ids.
 * The Branch's rows are read back before navigating so its canvas opens complete.
 */
export function useCreateBranch(projectSlug: string) {
  const scope = useCollectionScope();
  const navigate = useNavigate();
  return useMutation({
    mutationFn: async (input: CreateBranch) => {
      const { data } = await createBranchServerFn({ data: input });
      await Promise.all(branchTables.map((get) => reconcileCollection(get(input.organizationSlug, scope))));
      refetchEnvironmentChangeStates(input.organizationSlug, scope);
      return data;
    },
    onSuccess: (data, input) => navigate(getDashboardDestination({
      kind: "environment", organizationSlug: input.organizationSlug, projectSlug, environmentSlug: data.environment.namespace,
    }, "canvas")),
  });
}

/**
 * Stages the picked rows in the Destination, then opens its canvas, whose bottom bar shows them. Awaited: it is
 * destructive. Pending edits to the Destination save first, so the Merge checks the revision they leave.
 */
export function useMergeBranch(input: { organizationSlug: string; projectSlug: string; branchName: string; destination: { id: string; namespace: string } }) {
  const scope = useCollectionScope();
  const navigate = useNavigate();
  const queue = useEnvironmentDocumentQueue(input.organizationSlug);
  return useMutation({
    mutationFn: async (merge: Omit<MergeBranch, "organizationSlug" | "destinationRevision">) => {
      await queue.settled(input.destination.id);
      const destinationRevision = getEnvironmentsCollection(input.organizationSlug, scope).get(input.destination.id)?.revision;
      if (!destinationRevision) throw new Error("Still loading. Try again.");
      const { data } = await mergeBranchServerFn({ data: {
        organizationSlug: input.organizationSlug, branchEnvironmentId: merge.branchEnvironmentId, destinationRevision,
        review: merge.review, picks: merge.picks, thenClose: merge.thenClose,
      } });
      await Promise.all(branchTables.map((get) => reconcileCollection(get(input.organizationSlug, scope))));
      refetchEnvironmentChangeStates(input.organizationSlug, scope);
      return data;
    },
    onSuccess: (data, merge) => {
      if (merge.thenClose && !data.closed) toast.warning(`${input.branchName} is still open`, { description: "It couldn't close. Close it from its settings." });
      return navigate(getDashboardDestination({
        kind: "environment", organizationSlug: input.organizationSlug, projectSlug: input.projectSlug, environmentSlug: input.destination.namespace,
      }, "canvas"));
    },
  });
}

/**
 * Update a Branch from its Parent, and Make it an Own Copy of a Live Node. Queued behind pending edits like Discard, so
 * each saves against their revision; the queue toasts a failure. The rows they add are read back before they count as saved.
 */
export function useUpdateBranch(organizationSlug: string) {
  const scope = useCollectionScope();
  const queue = useEnvironmentDocumentQueue(organizationSlug);
  const afterSave = async () => {
    await Promise.all(branchTables.map((get) => reconcileCollection(get(organizationSlug, scope))));
  };
  return {
    update: (environmentId: string) => queue.enqueue({
      environmentId, failureMessage: "Could not update this branch.", afterSave,
      save: (revision) => updateBranchServerFn({ data: { organizationSlug, environmentId, revision } }),
    }),
    makeOwnCopy: (environmentId: string, lineageId: string) => queue.enqueue({
      environmentId, failureMessage: "Could not make it an own copy.", afterSave,
      save: (revision) => makeOwnCopyServerFn({ data: { organizationSlug, environmentId, revision, lineageId } }),
    }),
  };
}
