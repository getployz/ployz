import { useState } from "react";
import { reconcileDeploymentCollections } from "#/modules/deployments/deployment.collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { discardEnvironmentChangesServerFn } from "#/modules/environment-design/working-document-restore.functions";
import type { DiscardEnvironmentChangesInput } from "#/modules/environment-design/working-document-restore";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  getEnvironmentsCollection,
} from "#/collections/collections";
import type {
  CanvasEnvironmentChangeGroup,
  CanvasEnvironmentChangeState,
} from "#/modules/environment-design/canvas-environment-change-state";
import type {
  ReviewedPublicationInput,
  EnvironmentPublicationSubmissionOutcome,
} from "#/modules/deployments/deployment-contract";
import type {
  EnvironmentSavedStateBasis,
} from "#/modules/environment-design/saved-state";
import { getDeployTargetPreflight } from "#/modules/runtime/deploy-target-preflight";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  submitReviewedPublicationServerFn,
  prepareEnvironmentDestructiveVolumesServerFn,
} from "#/modules/deployments/deployment.functions";
import { serviceDeploymentKeys } from "#/modules/deployments/deployment-queries";
import type { PreparedDestructiveReview } from "#/components/destructive-volume/volume-destruction-confirmation-dialog";
import { prepareVolumeDestructionReview } from "#/components/destructive-volume/destructive-volume-review";
import {
  fingerprintReviewedEnvironmentWorkingState,
  projectReviewedEnvironmentWorkingState,
  type ReviewedEnvironmentPublication,
} from "#/modules/environment-design/working-state-review";

type EnvironmentRouteParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

type UseCanvasChangeActionsInput = {
  environmentId: string;
  params: EnvironmentRouteParams;
  changeState: CanvasEnvironmentChangeState;
  savedSnapshotSource: {
    kind: "saved";
    environmentSavedStateSnapshotId: string;
  } | null;
  destructiveServiceIds: string[];
  deletedDeployedVolumeIds: string[];
  commitMessage: string;
  setCommitMessage: (message: string) => void;
  setDestructiveConfirmationOpen: (open: boolean) => void;
};

export function useCanvasChangeActions({
  environmentId,
  params,
  changeState,
  savedSnapshotSource,
  destructiveServiceIds,
  deletedDeployedVolumeIds,
  commitMessage,
  setCommitMessage,
  setDestructiveConfirmationOpen,
}: UseCanvasChangeActionsInput) {
  const [reviewAction, setReviewAction] = useState<"save" | "deploy">("save");
  const collectionScope = useCollectionScope();
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  function workingReview() {
    if (!document) throw new Error("Environment is not loaded.");
    return projectReviewedEnvironmentWorkingState(document);
  }
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const environments = getEnvironmentsCollection(params.organizationSlug, collectionScope);
  const runtime = useRuntimeLens(params.organizationSlug);
  const deployTargetPreflight = getDeployTargetPreflight({
    status: runtime.status,
    machineCount: runtime.machines.length,
    error: runtime.error,
    isLoading: runtime.isLoading,
  });
  const savedStateBasis: EnvironmentSavedStateBasis = savedSnapshotSource
    ? {
        kind: "saved_revision",
        savedStateSnapshotId:
          savedSnapshotSource.environmentSavedStateSnapshotId,
      }
    : { kind: "no_saved_state" };
  const submitPublication = useServerFn(
    submitReviewedPublicationServerFn,
  );
  const discard = useServerFn(discardEnvironmentChangesServerFn);
  const prepareEnvironmentDestructiveVolumes = useServerFn(
    prepareEnvironmentDestructiveVolumesServerFn,
  );
  const publicationMutation = useMutation({
    mutationFn: async (input: { intent: ReviewedPublicationInput["intent"]; review: ReviewedEnvironmentPublication }) => {
      const result: EnvironmentPublicationSubmissionOutcome =
        await submitPublication({ data: { ...params, message: commitMessage, ...input } });

      await reconcileDeploymentCollections(params.organizationSlug, collectionScope);
      await queryClient.invalidateQueries({
        queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(
          params.organizationSlug,
        ),
      });

      if (result.state === "attempt_dispatch_failed") {
        toast.error("Changes saved, but deployment could not start. Review the failed deployment before retrying.");
      }
      if (result.state !== "review_updated_evidence") setCommitMessage("");
      return result;
    },
  });

  async function discardChanges(command: DiscardEnvironmentChangesInput["command"]) {
    try {
      if (!document) throw new Error("Environment is not loaded.");
      const result = await discard({ data: {
        organizationSlug: params.organizationSlug, environmentId, revision: document.revision,
        savedStateBasis, headToken: changeState.headToken, command,
      } });
      await environments.writeCommitted(result.data);
      await reconcileDeploymentCollections(params.organizationSlug, collectionScope);
      await queryClient.invalidateQueries({
        queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(params.organizationSlug),
      });
      return true;
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Could not discard changes.");
      return false;
    }
  }

  function discardAllChanges() {
    return discardChanges({ kind: "all" });
  }

  function discardNodeChanges(group: CanvasEnvironmentChangeGroup) {
    return discardChanges({ kind: "node", nodeType: group.nodeType, nodeId: group.nodeId });
  }

  function discardRowChange(group: CanvasEnvironmentChangeGroup, path: string) {
    return discardChanges({ kind: "node", nodeType: group.nodeType, nodeId: group.nodeId, path });
  }

  function deployTargetIsAvailable() {
    if (!deployTargetPreflight.ok) {
      toast.error(deployTargetPreflight.title, {
        description: deployTargetPreflight.description,
        action:
          deployTargetPreflight.action === "add_server"
            ? {
                label: "Add machine",
                onClick: () => {
                  void navigate({
                    to: "/cloud/$organizationSlug/~/servers",
                    params: { organizationSlug: params.organizationSlug },
                  });
                },
              }
            : undefined,
      });
      return false;
    }
    return true;
  }

  async function requestPublication(action: "save" | "deploy") {
    if (action === "deploy" && !deployTargetIsAvailable()) return;
    setReviewAction(action);
    if (destructiveServiceIds.length > 0 || deletedDeployedVolumeIds.length > 0) {
      setDestructiveConfirmationOpen(true);
      return;
    }
    try {
      await publicationMutation.mutateAsync({
        intent: action === "deploy" ? "manual_deploy" : "save",
        review: {
          savedStateBasis,
          workingStateFingerprint: await fingerprintReviewedEnvironmentWorkingState(workingReview()),
          destructiveServiceIds: [],
          destructiveVolumeReviews: [],
        },
      });
    } catch (error) {
      toast.error(error instanceof Error ? error.message : `Could not ${action} the changes.`);
    }
  }

  function requestDeploy() {
    return requestPublication("deploy");
  }

  function requestSave() {
    return requestPublication("save");
  }

  async function prepareDestructiveReview() {
    const reviewedMutation = {
      savedStateBasis,
      workingStateFingerprint:
        await fingerprintReviewedEnvironmentWorkingState(
          workingReview(),
        ),
      serviceIds: [...destructiveServiceIds],
      volumeIds: [...deletedDeployedVolumeIds],
    };
    const reviews = await prepareEnvironmentDestructiveVolumes({
        data: {
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
        },
      });
    return {
      ...prepareVolumeDestructionReview({
        reviews,
        expectedResourceIds: reviewedMutation.volumeIds,
        expectedNamespaceId: params.environmentSlug,
      }),
      reviewedMutation,
    };
  }

  async function confirmDestructiveAction(
    preparation: PreparedDestructiveReview,
  ) {
    if (!preparation.reviewedMutation) {
      throw new Error("The destructive action is missing its reviewed mutation.");
    }
    const reviewedMutation = preparation.reviewedMutation;
    const outcome = await publicationMutation.mutateAsync({
      intent: reviewAction === "deploy" ? "manual_deploy" : "save",
      review: {
        savedStateBasis: reviewedMutation.savedStateBasis,
        workingStateFingerprint: reviewedMutation.workingStateFingerprint,
        destructiveServiceIds: reviewedMutation.serviceIds,
        destructiveVolumeReviews: preparation.reviews,
      },
    });
    if (outcome.state === "review_updated_evidence") {
      return {
        state: "review_updated_evidence" as const,
        preparation: {
          ...prepareVolumeDestructionReview({
            reviews: outcome.freshReviews,
            expectedResourceIds: reviewedMutation.volumeIds,
            expectedNamespaceId: params.environmentSlug,
          }),
          reviewedMutation,
        },
      };
    }
    return { state: "submitted" as const };
  }

  return {
    reviewAction,
    discardAllChanges,
    discardNodeChanges,
    discardRowChange,
    requestSave,
    isSubmittingDeploymentSnapshot: publicationMutation.isPending,
    requestDeploy,
    prepareDestructiveReview,
    confirmDestructiveAction,
  };
}
