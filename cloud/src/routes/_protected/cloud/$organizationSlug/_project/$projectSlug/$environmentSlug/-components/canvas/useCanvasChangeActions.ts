import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  getRawEnvironmentResourcesCollection,
  getRawServiceVariableGroupAttachmentsCollection,
  getRawServicesCollection,
  getRawVariablesCollection,
  getServiceVolumeAttachmentsCollection,
  getEnvironmentDeploymentsCollection,
} from "#/electric/collections";
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
import {
  discardVolumeResourceServerFn,
  restoreVariableGroupResourceSnapshotServerFn,
} from "#/modules/environment-design/environment-resources.functions";
import { deleteVariableGroupResourceServerFn } from "#/modules/environment-design/resource-functions";
import { getDeployTargetPreflight } from "#/modules/runtime/deploy-target-preflight";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import {
  createEnvironmentDeploymentSnapshotServerFn,
  discardEnvironmentSavedChangeServerFn,
  prepareEnvironmentDestructiveVolumesServerFn,
} from "#/modules/deployments/deployment.functions";
import { serviceDeploymentKeys } from "#/modules/deployments/deployment-queries";
import { discardServiceDeploymentDiffPath } from "#/modules/services/service-deployment-diff/mutations";
import type { ServiceDeploymentDiffPath } from "#/modules/services/service-deployment-diff/fields";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { EnvironmentSnapshotSource } from "#/modules/environment-design/environment-snapshot-source";
import { deleteServicesServerFn } from "#/modules/environment-design/service-functions";
import { restoreServiceWorkingIntentServerFn } from "#/modules/services/services.functions";
import { useServiceWriter } from "#/modules/services/services.collection";
import type { PreparedDestructiveReview } from "#/components/destructive-volume/volume-destruction-confirmation-dialog";
import { prepareVolumeDestructionReview } from "#/components/destructive-volume/destructive-volume-review";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
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
  servicesWithBoundEnv: EnvironmentServiceViewRecord[];
  environmentResources: VariableGroupResourceRecord[];
  volumeResources: VolumeResourceRecord[];
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
  servicesWithBoundEnv,
  environmentResources,
  volumeResources,
  destructiveServiceIds,
  deletedDeployedVolumeIds,
  commitMessage,
  setCommitMessage,
  setDestructiveConfirmationOpen,
}: UseCanvasChangeActionsInput) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const serviceWriter = useServiceWriter(params.organizationSlug);
  const rawServices = getRawServicesCollection(params.organizationSlug);
  const rawVariables = getRawVariablesCollection(params.organizationSlug);
  const rawVariableGroupAttachments =
    getRawServiceVariableGroupAttachmentsCollection(params.organizationSlug);
  const rawVolumeAttachments = getServiceVolumeAttachmentsCollection(
    params.organizationSlug,
  );
  const rawResources = getRawEnvironmentResourcesCollection(
    params.organizationSlug,
  );
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
  const restoreVariableGroupResourceSnapshot = useServerFn(
    restoreVariableGroupResourceSnapshotServerFn,
  );
  const deleteVariableGroupResource = useServerFn(
    deleteVariableGroupResourceServerFn,
  );
  const discardVolumeResource = useServerFn(discardVolumeResourceServerFn);
  const createDeploymentSnapshot = useServerFn(
    createEnvironmentDeploymentSnapshotServerFn,
  );
  const discardSavedChange = useServerFn(
    discardEnvironmentSavedChangeServerFn,
  );
  const prepareEnvironmentDestructiveVolumes = useServerFn(
    prepareEnvironmentDestructiveVolumesServerFn,
  );
  const deleteServices = useServerFn(deleteServicesServerFn);
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
          projectReviewedEnvironmentWorkingState({
            services: servicesWithBoundEnv,
            variableGroups: environmentResources,
            volumes: volumeResources,
          }),
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

      if (result.state === "deployment_queued") {
        await getEnvironmentDeploymentsCollection(
          params.organizationSlug,
        ).utils.awaitTxId(result.txid);
      }

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
      txid: number;
      data: { savedStateSnapshotId: string };
    } = await discardSavedChange({
        data: {
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
          command,
        },
      });
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
    for (const plan of changeState.discardAllPlan.nodes) {
      await discardWorkingNodePlan(plan.working, workingSnapshotSource);
    }
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
    if (plan.node.type === "variable_group") {
      if (plan.kind === "delete") {
        const receipt = await deleteVariableGroupResource({
          data: {
            organizationSlug: params.organizationSlug,
            environmentId,
            resourceId: plan.node.id,
          },
        });
        await rawResources.utils.awaitTxId(receipt.txid);
        return;
      }
      if (!snapshotSource) return;
      const receipt = await restoreVariableGroupResourceSnapshot({
          data: {
            organizationSlug: params.organizationSlug,
            environmentId,
            resourceId: plan.node.id,
            snapshotSource,
          },
        });
      await rawResources.utils.awaitTxId(receipt.txid);
      return;
    }

    if (plan.node.type === "volume") {
      const receipt = await discardVolumeResource({
          data: {
            organizationSlug: params.organizationSlug,
            environmentId,
            resourceId: plan.node.id,
            snapshotSource,
          },
        });
      await rawResources.utils.awaitTxId(receipt.txid);
      return;
    }

    if (plan.kind === "delete") {
      const receipt = await deleteServices({
        data: {
          organizationSlug: params.organizationSlug,
          environmentId,
          serviceIds: [plan.node.id],
        },
      });
      await rawServices.utils.awaitTxId(receipt.txid);
      return;
    }

    if (!snapshotSource || snapshotSource.kind !== "saved") {
      throw new Error(
        "A complete Service Working Intent reset requires Saved provenance.",
      );
    }
    const receipt = await restoreServiceWorkingIntentServerFn({
        data: {
          organizationSlug: params.organizationSlug,
          environmentId,
          serviceId: plan.node.id,
          savedStateSnapshotId:
            snapshotSource.environmentSavedStateSnapshotId,
        },
      });
    const waits: Promise<unknown>[] = [
      rawServices.utils.awaitTxId(receipt.txid),
    ];
    if (receipt.data.changedCollections.variables) {
      waits.push(rawVariables.utils.awaitTxId(receipt.txid));
    }
    if (receipt.data.changedCollections.variableGroupAttachments) {
      waits.push(rawVariableGroupAttachments.utils.awaitTxId(receipt.txid));
    }
    if (receipt.data.changedCollections.volumeAttachments) {
      waits.push(rawVolumeAttachments.utils.awaitTxId(receipt.txid));
    }
    await Promise.all(waits);
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

    const transaction = serviceWriter.update(group.nodeId, (draft) => {
      // SAFETY: this path only runs for service groups; discard plans store a node-union config, and the row path is a service deployment diff path.
      discardServiceDeploymentDiffPath({
        draft,
        baseline: plan.config as ServiceDeploymentConfig,
        path: path as ServiceDeploymentDiffPath,
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
          projectReviewedEnvironmentWorkingState({
            services: servicesWithBoundEnv,
            variableGroups: environmentResources,
            volumes: volumeResources,
          }),
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
