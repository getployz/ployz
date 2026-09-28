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
import { createBranchServerFn, makeOwnCopyServerFn, saveBranchServerFn, updateBranchServerFn } from "./branch-functions";
import type { CreateBranch, SaveBranch } from "./branch-schemas";

/** Every Org Store table creating, saving or updating a Branch writes rows into. */
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
    }, "architecture")),
  });
}

/**
 * Save: stages the picked rows in the Destination, then opens its canvas, whose bottom bar shows them as changes to
 * deploy. Awaited: it is destructive. Pending edits to the Destination save first, so the Save checks the revision they
 * leave.
 */
export function useSaveBranch(input: {
  organizationSlug: string; projectSlug: string; branchName: string; destination: { id: string; name: string; namespace: string };
}) {
  const scope = useCollectionScope();
  const navigate = useNavigate();
  const queue = useEnvironmentDocumentQueue(input.organizationSlug);
  return useMutation({
    mutationFn: async (save: Omit<SaveBranch, "organizationSlug" | "destinationRevision">) => {
      await queue.settled(input.destination.id);
      const destinationRevision = getEnvironmentsCollection(input.organizationSlug, scope).get(input.destination.id)?.revision;
      if (!destinationRevision) throw new Error("Still loading. Try again.");
      const { data } = await saveBranchServerFn({ data: { ...save, organizationSlug: input.organizationSlug, destinationRevision } });
      await Promise.all(branchTables.map((get) => reconcileCollection(get(input.organizationSlug, scope))));
      refetchEnvironmentChangeStates(input.organizationSlug, scope);
      return data;
    },
    onSuccess: (data, save) => {
      toast.success(`Saved to ${input.destination.name}`, { description: data.closed ? `${input.branchName} deleted` : undefined });
      if (save.thenDelete && !data.closed) toast.warning(`${input.branchName} is still here`, { description: "It couldn't be deleted. Delete it from its settings." });
      return navigate(getDashboardDestination({
        kind: "environment", organizationSlug: input.organizationSlug, projectSlug: input.projectSlug, environmentSlug: input.destination.namespace,
      }, "architecture"));
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
      environmentId, failureMessage: "Could not make it separate.", afterSave,
      save: (revision) => makeOwnCopyServerFn({ data: { organizationSlug, environmentId, revision, lineageId } }),
    }),
  };
}
