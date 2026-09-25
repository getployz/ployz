import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { EnvironmentDeploymentSummary } from "./deployment-contract";
import { reconcileDeploymentCollections } from "./deployment.collection";
import { dispatchQueuedEnvironmentDeploymentServerFn, retryEnvironmentDeploymentServerFn } from "./deployment.functions";

/** Runs one deployment command against the attempt's environment, then reconciles the deployment rows it changed. */
function useDeploymentCommand(deployment: EnvironmentDeploymentSummary, messages: { success: string; failure: string },
  run: (target: { organizationSlug: string; projectSlug: string; environmentSlug: string }) => Promise<string | void>) {
  const [isRunning, setIsRunning] = useState(false);
  const { organizationSlug } = useParams({ strict: false });
  const collectionScope = useCollectionScope();
  async function start() {
    if (!organizationSlug) return;
    setIsRunning(true);
    try {
      const success = await run({ organizationSlug, projectSlug: deployment.projectSlug, environmentSlug: deployment.environmentSlug });
      await reconcileDeploymentCollections(organizationSlug, collectionScope);
      toast.success(success ?? messages.success);
    } catch {
      toast.error(messages.failure);
    } finally {
      setIsRunning(false);
    }
  }
  return [start, isRunning] as const;
}

/** Retry re-admits a failed attempt's frozen target. */
export function useRetryDeployment(deployment: EnvironmentDeploymentSummary) {
  return useDeploymentCommand(deployment, { success: "Deployment retry queued.", failure: "Could not retry this deployment." },
    async (target) => { await retryEnvironmentDeploymentServerFn({ data: { ...target, failedDeploymentId: deployment.id } }); });
}

/** Deploy now dispatches an attempt queued for its environment's next trigger; behind a building attempt it waits. */
export function useDeployQueuedNow(deployment: EnvironmentDeploymentSummary) {
  return useDeploymentCommand(deployment, { success: "Deployment requested.", failure: "Could not request this deployment." },
    async (target) => {
      const dispatched = await dispatchQueuedEnvironmentDeploymentServerFn({ data: target });
      if (dispatched.state === "pending") return "Waiting for the current build.";
    });
}
