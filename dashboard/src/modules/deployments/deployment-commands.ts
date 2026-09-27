import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { openStartedDeployments } from "#/auth/open-started-deployments";
import { getEnvironmentDocumentsCollection } from "#/modules/environment-design/environment-document.collection";
import { useEnvironmentDocumentQueue } from "#/modules/environment-design/environment-document-edit";
import { fingerprintReviewedEnvironmentWorkingState, projectReviewedEnvironmentWorkingState } from "#/modules/environment-design/working-state-review";
import type { EnvironmentDeploymentSummary } from "./deployment-contract";
import { reconcileDeploymentCollections } from "./deployment.collection";
import {
  dispatchQueuedEnvironmentDeploymentServerFn, retryEnvironmentDeploymentServerFn, submitReviewedPublicationServerFn,
} from "./deployment.functions";

/** Runs one deployment command against the attempt's environment, then reconciles the deployment rows it changed. */
function useDeploymentCommand<T>(deployment: EnvironmentDeploymentSummary, messages: { success: (result: T) => string; failure: string },
  run: (target: { organizationSlug: string; projectSlug: string; environmentSlug: string }) => Promise<T>, onDone?: (result: T) => void) {
  const [isRunning, setIsRunning] = useState(false);
  const { organizationSlug } = useParams({ strict: false });
  const collectionScope = useCollectionScope();
  async function start() {
    if (!organizationSlug) return;
    setIsRunning(true);
    try {
      const result = await run({ organizationSlug, projectSlug: deployment.projectSlug, environmentSlug: deployment.environmentSlug });
      await reconcileDeploymentCollections(organizationSlug, collectionScope);
      toast.success(messages.success(result));
      onDone?.(result);
    } catch {
      toast.error(messages.failure);
    } finally {
      setIsRunning(false);
    }
  }
  return [start, isRunning] as const;
}

/** Retry re-admits a failed attempt's frozen target, then opens the Deployment Page of the new attempt the user just started. */
export function useRetryDeployment(deployment: EnvironmentDeploymentSummary) {
  const navigate = useNavigate();
  const { organizationSlug = "" } = useParams({ strict: false });
  return useDeploymentCommand(deployment, { success: () => "Deployment retry queued.", failure: "Could not retry this deployment." },
    async (target) => (await retryEnvironmentDeploymentServerFn({ data: { ...target, failedDeploymentId: deployment.id } })).data.environmentDeploymentId,
    (retried) => void navigate({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId",
      params: { organizationSlug, projectSlug: deployment.projectSlug, environmentSlug: deployment.environmentSlug, deploymentId: retried },
    }));
}

/** Deploy now dispatches an attempt queued for its environment's next trigger; behind a building attempt it waits. */
export function useDeployQueuedNow(deployment: EnvironmentDeploymentSummary) {
  return useDeploymentCommand(deployment, {
    success: (state) => state === "pending" ? "Waiting for the current build." : "Deployment requested.",
    failure: "Could not request this deployment.",
  }, async (target) => (await dispatchQueuedEnvironmentDeploymentServerFn({ data: target })).state);
}

/**
 * "Deploy this environment": a starting point's first deployment, the manual deploy path over its Working State once
 * pending edits save. It has no saved state and nothing deployed to remove. Opens the new attempt's Deployment Page
 * unless the user opted out.
 */
export function useDeployStartingPoint({ organizationSlug, projectSlug, environmentSlug, environmentId }: {
  organizationSlug: string; projectSlug: string; environmentSlug: string; environmentId: string;
}) {
  const environment = { organizationSlug, projectSlug, environmentSlug };
  const scope = useCollectionScope();
  const queue = useEnvironmentDocumentQueue(organizationSlug);
  const navigate = useNavigate();
  return useMutation({
    mutationFn: async () => {
      await queue.settled(environmentId);
      const document = getEnvironmentDocumentsCollection(organizationSlug, scope).get(environmentId);
      if (!document) throw new Error("Environment is not loaded.");
      const result = await submitReviewedPublicationServerFn({ data: { organizationSlug, projectSlug, environmentSlug, intent: "manual_deploy", review: {
        savedStateBasis: { kind: "no_saved_state" },
        workingStateFingerprint: await fingerprintReviewedEnvironmentWorkingState(projectReviewedEnvironmentWorkingState(document)),
        destructiveServiceIds: [],
        destructiveVolumeReviews: [],
      } } });
      await reconcileDeploymentCollections(organizationSlug, scope);
      return result;
    },
    onSuccess: (result) => {
      if (result.state === "deployment_queued") {
        if (openStartedDeployments()) void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId", params: { ...environment, deploymentId: result.deploymentId } });
      } else toast.error("Saved, but the deployment could not start. Review the failed deployment before retrying.");
    },
  });
}
