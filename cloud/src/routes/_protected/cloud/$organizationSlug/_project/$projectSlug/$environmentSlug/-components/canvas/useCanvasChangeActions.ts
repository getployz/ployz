import { reconcileDeploymentCollections } from "#/modules/deployments/deployment-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { restoreWorkingDocumentServerFn } from "#/modules/environment-design/working-document-restore.functions";
import { createWorkingSettingRestoreAction } from "#/modules/environment-design/working-setting-restore-action";
import { parseServiceConfig } from "@ployz/sdk/config";
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
  CanvasWorkingNodeDiscardPlan,
} from "#/modules/environment-design/canvas-environment-change-state";
import { toCanvasWorkingNodeDiscardPlan } from "#/modules/environment-design/canvas-environment-change-state";
import type {
  DestructiveVolumeReview,
  EnvironmentPublicationSubmissionOutcome,
} from "#/modules/deployments/deployment-contract";
import type {
  EnvironmentSavedStateBasis,
  EnvironmentSavedStateDiscardCommand,
} from "#/modules/environment-design/saved-state";
import { getDeployTargetPreflight } from "#/modules/runtime/deploy-target-preflight";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  createEnvironmentDeploymentSnapshotServerFn,
  discardEnvironmentSavedChangeServerFn,
  prepareEnvironmentDestructiveVolumesServerFn,
} from "#/modules/deployments/deployment.functions";
import { serviceDeploymentKeys } from "#/modules/deployments/deployment-queries";
import { discardServiceDeploymentDiffPath } from "#/modules/services/service-deployment-diff/mutations";
import type { EnvironmentSnapshotSource } from "#/modules/environment-design/environment-snapshot-source";
import { useServiceWriter } from "#/modules/services/services.collection";
import type { PreparedDestructiveReview } from "#/components/destructive-volume/volume-destruction-confirmation-dialog";
import { prepareVolumeDestructionReview } from "#/components/destructive-volume/destructive-volume-review";
import {
  fingerprintReviewedEnvironmentWorkingState,
  projectReviewedEnvironmentWorkingState,
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
  const collectionScope = useCollectionScope();
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  function workingReview() {
    if (!document) throw new Error("Environment is not loaded.");
    return projectReviewedEnvironmentWorkingState(document);
  }
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const serviceWriter = useServiceWriter(params.organizationSlug);
  const environments = getEnvironmentsCollection(params.organizationSlug, collectionScope);
  const restoreWorkingSetting = createWorkingSettingRestoreAction({
    environments, environmentId, organizationSlug: params.organizationSlug,
    restore: restoreWorkingDocumentServerFn,
  });
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
  const createDeploymentSnapshot = useServerFn(
    createEnvironmentDeploymentSnapshotServerFn,
  );
  const discardSavedChange = useServerFn(
    discardEnvironmentSavedChangeServerFn,
  );
  const prepareEnvironmentDestructiveVolumes = useServerFn(
    prepareEnvironmentDestructiveVolumesServerFn,
  );
  const createDeploymentSnapshotMutation = useMutation({
    mutationFn: async (input: {
      deploy: boolean;
      message: string;
      savedStateBasis: EnvironmentSavedStateBasis;
      destructiveServiceIds: string[];
      destructiveVolumeReviews: DestructiveVolumeReview[];
      reviewedWorkingStateFingerprint?: string;
    }) => {
      const reviewedWorkingStateFingerprint =
        input.reviewedWorkingStateFingerprint ??
        (await fingerprintReviewedEnvironmentWorkingState(
          workingReview(),
        ));
      const result: EnvironmentPublicationSubmissionOutcome =
        await createDeploymentSnapshot({
          data: {
            organizationSlug: params.organizationSlug,
            projectSlug: params.projectSlug,
            environmentSlug: params.environmentSlug,
            message: input.message,
            deploy: input.deploy,
            savedStateBasis: input.savedStateBasis,
            reviewedWorkingStateFingerprint,
            destructiveServiceIds: input.destructiveServiceIds,
            destructiveVolumeReviews: input.destructiveVolumeReviews,
          },
        });

      await reconcileDeploymentCollections(params.organizationSlug, collectionScope);
      await queryClient.invalidateQueries({
        queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(
          params.organizationSlug,
        ),
      });

      return result;
    },
  });

  const discardableGroups = [
    ...changeState.slices.unsaved.groups,
    ...changeState.slices.pending.groups,
  ].filter((group) => group.canDiscard || group.rows.some((row) => row.canDiscard));
  const discardableGroupsByNodeId = new Map(
    discardableGroups.map((group) => [group.nodeId, group]),
  );

  async function refreshExplicitChangeState() {
    await queryClient.invalidateQueries({
      queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(
        params.organizationSlug,
      ),
    });
  }

  async function discardPendingSavedChange(
    command: EnvironmentSavedStateDiscardCommand,
  ) {
    const receipt: {
      data: { savedStateSnapshotId: string };
    } = await discardSavedChange({
        data: {
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
          command,
        },
      });
    await reconcileDeploymentCollections(params.organizationSlug, collectionScope);
    await refreshExplicitChangeState();
    return receipt;
  }

  async function discardAllChanges() {
    let workingSnapshotSource: EnvironmentSnapshotSource | null =
      savedSnapshotSource;
    if (changeState.discardAllPlan.savedCommand) {
      const receipt = await discardPendingSavedChange(
        changeState.discardAllPlan.savedCommand,
      );
      workingSnapshotSource = {
        kind: "saved",
        environmentSavedStateSnapshotId:
          receipt.data.savedStateSnapshotId,
      };
    }
    const document = environments.get(environmentId);
    if (!document) throw new Error("Environment is not loaded.");
    const result = await restoreWorkingDocumentServerFn({ data: {
      organizationSlug: params.organizationSlug, environmentId, revision: document.revision,
      snapshotSource: workingSnapshotSource, command: { kind: "all" },
    } });
    await environments.writeCommitted(result.data);
  }

  async function discardServiceChanges(serviceId: string) {
    const group = discardableGroupsByNodeId.get(serviceId);
    if (!group || group.nodeType !== "service") return;
    await discardNodeChanges(group);
  }

  async function discardVariableGroupChanges(
    group: CanvasEnvironmentChangeGroup,
  ) {
    if (group.nodeType !== "variable_group") {
      return;
    }

    const plan = group.projectedChange.discardPlan;
    if (!plan || plan.target !== "working") return;
    await discardWorkingNodePlan(
      toCanvasWorkingNodeDiscardPlan(plan),
      savedSnapshotSource,
    );
  }

  async function discardWorkingNodePlan(
    plan: CanvasWorkingNodeDiscardPlan,
    snapshotSource: EnvironmentSnapshotSource | null,
  ) {
    const document = environments.get(environmentId);
    if (!document) throw new Error("Environment is not loaded.");
    const result = await restoreWorkingDocumentServerFn({ data: {
      organizationSlug: params.organizationSlug, environmentId, revision: document.revision,
      snapshotSource: plan.kind === "delete" ? null : snapshotSource,
      command: { kind: "node", nodeType: plan.node.type, nodeId: plan.node.id },
    } });
    await environments.writeCommitted(result.data);
  }

  async function discardVolumeChanges(group: CanvasEnvironmentChangeGroup) {
    if (group.nodeType !== "volume") {
      return;
    }

    const plan = group.projectedChange.discardPlan;
    if (!plan || plan.target !== "working") return;
    await discardWorkingNodePlan(
      toCanvasWorkingNodeDiscardPlan(plan),
      savedSnapshotSource,
    );
  }

  async function discardNodeChanges(group: CanvasEnvironmentChangeGroup) {
    const plan = group.projectedChange.discardPlan;
    if (!plan) return;

    if (plan.target === "saved") {
      await discardPendingSavedChange({
        kind: "discard",
        basis: plan.basis,
        operations: [
          {
            kind: "node",
            nodeType: plan.node.type,
            nodeId: plan.node.id,
          },
        ],
      });
      return;
    }

    await discardWorkingNodePlan(
      toCanvasWorkingNodeDiscardPlan(plan),
      savedSnapshotSource,
    );
  }

  async function discardRowChange(
    group: CanvasEnvironmentChangeGroup,
    path: string,
  ) {
    if (group.nodeType !== "service" || group.slice === "drift") return;
    const setting = group.projectedChange.settings.find(
      (candidate) => candidate.owner.setting === path,
    );
    const plan = setting?.discardPlan;
    if (!plan) return;

    if (plan.target === "saved") {
      await discardPendingSavedChange({
        kind: "discard",
        basis: plan.basis,
        operations: [
          {
            kind: "setting",
            nodeType: "service",
            nodeId: group.nodeId,
            setting: plan.owner.setting,
          },
        ],
      });
      return;
    }

    if (path === "variableGroupAttachments" || path === "source.credentials" || path === "source") {
      const current = environments.get(environmentId);
      if (!current) throw new Error("Environment is not loaded.");
      await restoreWorkingSetting({ serviceId: group.nodeId, revision: current.revision, path,
        baseline: parseServiceConfig(plan.config),
        snapshotSource: setting?.baselineSource?.role === "node_introduction"
          ? { kind: "introduction" } : savedSnapshotSource,
      }).isPersisted.promise;
      return;
    }

    const transaction = serviceWriter.update(group.nodeId, (draft) => {
      // SAFETY: this path only runs for service groups; discard plans store a node-union config, and the row path is a service deployment diff path.
      discardServiceDeploymentDiffPath({
        draft,
        baseline: parseServiceConfig(plan.config),
        path,
      });
    });

    await transaction.isPersisted.promise;
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

  async function submitDeployment() {
    const outcome = await createDeploymentSnapshotMutation.mutateAsync({
      deploy: true,
      message: commitMessage,
      savedStateBasis,
      destructiveServiceIds: [],
      destructiveVolumeReviews: [],
    });
    if (outcome.state === "deployment_queued") setCommitMessage("");
    return outcome;
  }

  async function handleDeploy() {
    if (!deployTargetIsAvailable()) return;
    try {
      await submitDeployment();
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "Failed to queue the desired state snapshot.",
      );
    }
  }

  async function handleSaveWithoutDeploying() {
    try {
      await createDeploymentSnapshotMutation.mutateAsync({
        deploy: false,
        message: commitMessage,
        savedStateBasis,
        destructiveServiceIds: [],
        destructiveVolumeReviews: [],
      });

      setCommitMessage("");
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "Failed to save the desired state snapshot.",
      );
    }
  }

  function requestDeploy() {
    void handleDeploy();
  }

  function requestSave() {
    if (
      destructiveServiceIds.length > 0 ||
      deletedDeployedVolumeIds.length > 0
    ) {
      setDestructiveConfirmationOpen(true);
      return;
    }
    void handleSaveWithoutDeploying();
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
      throw new Error("The destructive Save is missing its reviewed mutation.");
    }
    const reviewedMutation = preparation.reviewedMutation;
    const outcome = await createDeploymentSnapshotMutation.mutateAsync({
      deploy: false,
      message: commitMessage,
      savedStateBasis: reviewedMutation.savedStateBasis,
      reviewedWorkingStateFingerprint:
        reviewedMutation.workingStateFingerprint,
      destructiveServiceIds: reviewedMutation.serviceIds,
      destructiveVolumeReviews: preparation.reviews,
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
    setCommitMessage("");
    return { state: "submitted" as const };
  }

  return {
    discardAllChanges,
    discardServiceChanges,
    discardVariableGroupChanges,
    discardVolumeChanges,
    discardNodeChanges,
    discardRowChange,
    requestSave,
    isSubmittingDeploymentSnapshot: createDeploymentSnapshotMutation.isPending,
    requestDeploy,
    prepareDestructiveReview,
    confirmDestructiveAction,
  };
}
