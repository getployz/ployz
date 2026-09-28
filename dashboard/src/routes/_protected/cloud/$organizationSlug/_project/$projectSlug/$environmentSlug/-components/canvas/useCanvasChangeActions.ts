import { useRef, useState } from "react";
import { useStillHere } from "#/hooks/use-still-here";
import { openStartedDeployments } from "#/auth/open-started-deployments";
import { reconcileDeploymentCollections } from "#/modules/deployments/deployment.collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocumentQueue } from "#/modules/environment-design/environment-document-edit";
import { getEnvironmentDocumentsCollection } from "#/modules/environment-design/environment-document.collection";
import { discardEnvironmentChangesServerFn } from "#/modules/environment-design/working-document-restore.functions";
import type { DiscardEnvironmentChangesInput } from "#/modules/environment-design/working-document-restore";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
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
import { prepareVolumeDestructionReview, type PreparedDestructiveReview } from "#/components/destructive-volume/destructive-volume-review";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
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
  /** Removals ask for a typed confirmation; otherwise the Review is the confirmation. */
  confirmsRemovals: boolean;
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
  confirmsRemovals,
  commitMessage,
  setCommitMessage,
  setDestructiveConfirmationOpen,
}: UseCanvasChangeActionsInput) {
  const [reviewAction, setReviewAction] = useState<"save" | "deploy">("save");
  // Set from the click through settling queued edits, fingerprinting and submitting: a second click in that window is
  // ignored, so it can't queue a second deployment. The ref guards re-entry; the state only renders Deploy disabled.
  const submittingRef = useRef(false);
  const [submitting, setSubmitting] = useState(false);
  const markHere = useStillHere();
  const collectionScope = useCollectionScope();
  const documents = getEnvironmentDocumentsCollection(params.organizationSlug, collectionScope);
  // Read at call time: after queued edits settle, the render-time document is stale.
  function workingReview() {
    const document = documents.get(environmentId);
    if (!document) throw new Error("Environment is not loaded.");
    return projectReviewedEnvironmentWorkingState(document);
  }
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  // Discard runs in the document save queue; publishing reviews the saved working state, so queued edits land first.
  const queue = useEnvironmentDocumentQueue(params.organizationSlug);
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
    mutationFn: async ({ intent, review, stillHere }: {
      intent: ReviewedPublicationInput["intent"]; review: ReviewedEnvironmentPublication;
      /** Whether the user is still where they clicked. */
      stillHere: () => boolean;
    }) => {
      const result: EnvironmentPublicationSubmissionOutcome =
        await submitPublication({ data: { ...params, message: commitMessage, intent, review } });

      await reconcileDeploymentCollections(params.organizationSlug, collectionScope);
      await queryClient.invalidateQueries({
        queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(
          params.organizationSlug,
        ),
      });

      // A manual Deploy opens its attempt unless the user opted out by leaving one they started while it ran, or has moved on.
      if (result.state === "deployment_queued" && openStartedDeployments() && stillHere()) {
        void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId: result.deploymentId } });
      }
      if (result.state === "attempt_dispatch_failed") {
        toast.error("Changes saved, but deployment could not start. Review the failed deployment before retrying.");
      }
      if (result.state !== "review_updated_evidence") setCommitMessage("");
      return result;
    },
  });

  // Discarding queues behind in-flight edits so it saves against their revision; the editor toasts failures.
  async function discardChanges(command: DiscardEnvironmentChangesInput["command"]) {
    try {
      await queue.enqueue({
        environmentId,
        save: (revision) => discard({ data: {
          organizationSlug: params.organizationSlug, environmentId, revision,
          savedStateBasis, headToken: changeState.headToken, command,
        } }),
        failureMessage: "Could not discard changes.",
        afterSave: async () => {
          await reconcileDeploymentCollections(params.organizationSlug, collectionScope);
          await queryClient.invalidateQueries({
            queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(params.organizationSlug),
          });
        },
      }).isPersisted.promise;
      return true;
    } catch {
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
                label: "Add server",
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
    if (submittingRef.current) return;
    if (action === "deploy" && !deployTargetIsAvailable()) return;
    setReviewAction(action);
    const destructive = destructiveServiceIds.length > 0 || deletedDeployedVolumeIds.length > 0;
    if (destructive && confirmsRemovals) {
      setDestructiveConfirmationOpen(true);
      return;
    }
    const stillHere = markHere();
    submittingRef.current = true;
    setSubmitting(true);
    try {
      if (destructive) {
        const outcome = await confirmDestructiveAction(await prepareDestructiveReview(), action, stillHere);
        // Evidence that moved since the Review needs a person to look again.
        if (outcome.state === "review_updated_evidence") toast.error(`Your servers changed. Review and ${action} again.`);
        return;
      }
      await queue.settled(environmentId);
      await publicationMutation.mutateAsync({
        intent: action === "deploy" ? "manual_deploy" : "save",
        review: {
          savedStateBasis,
          workingStateFingerprint: await fingerprintReviewedEnvironmentWorkingState(workingReview()),
          destructiveServiceIds: [],
          destructiveVolumeReviews: [],
        },
        stillHere,
      });
    } catch (error) {
      toast.error(error instanceof Error ? error.message : `Could not ${action} the changes.`);
    } finally {
      submittingRef.current = false;
      setSubmitting(false);
    }
  }

  function requestDeploy() {
    return requestPublication("deploy");
  }

  function requestSave() {
    return requestPublication("save");
  }

  async function prepareDestructiveReview() {
    await queue.settled(environmentId);
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
    action = reviewAction,
    stillHere = markHere(),
  ) {
    if (!preparation.reviewedMutation) {
      throw new Error("The destructive action is missing its reviewed mutation.");
    }
    const reviewedMutation = preparation.reviewedMutation;
    const outcome = await publicationMutation.mutateAsync({
      intent: action === "deploy" ? "manual_deploy" : "save",
      review: {
        savedStateBasis: reviewedMutation.savedStateBasis,
        workingStateFingerprint: reviewedMutation.workingStateFingerprint,
        destructiveServiceIds: reviewedMutation.serviceIds,
        destructiveVolumeReviews: preparation.reviews,
      },
      stillHere,
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
    isSubmittingDeploymentSnapshot: submitting || publicationMutation.isPending,
    requestDeploy,
    prepareDestructiveReview,
    confirmDestructiveAction,
  };
}
