import { useState } from "react";
import { useParams, useRouter } from "@tanstack/react-router";
import { useLiveQuery } from "@tanstack/react-db";
import { MoreVerticalIcon, XIcon } from "lucide-react";
import { toast } from "sonner";
import { CancelDeploymentDialog } from "#/components/cancel-deployment-dialog";
import { DeploymentStatusCard } from "#/components/deployment-status-card";
import { DeploymentLogs } from "#/components/deployment-logs";
import { reconcileDeploymentCollections } from "#/modules/deployments/deployment-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { VolumeRemoveAttemptHistory } from "#/components/volume-remove/deployment-volume-remove-history";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { getRawEnvironmentResourcesCollection } from "#/collections/collections";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { dispatchQueuedEnvironmentDeploymentServerFn, retryEnvironmentDeploymentServerFn } from "#/modules/deployments/deployment.functions";

export function DeploymentRow({ deployment, serviceId }: { deployment: EnvironmentDeploymentSummary; serviceId?: string }) {
  const [isOpen, setIsOpen] = useState(() => ["planning", "deploying"].includes(deployment.status));
  const [isRetrying, setIsRetrying] = useState(false);
  const [isDispatching, setIsDispatching] = useState(false);
  const router = useRouter();
  const { organizationSlug } = useParams({ strict: false });
  const collectionScope = useCollectionScope();
  const rawResources = getRawEnvironmentResourcesCollection(
    organizationSlug ?? "", collectionScope,
  );
  const { data: resourceRows = [] } = useLiveQuery({
    query: (q) =>
      q
        .from({ resource: rawResources })
        .select(({ resource }) => ({
          id: resource.id,
          implementationType: resource.implementationType,
        })),
  });
  const availableVolumeResourceIds = new Set(
    resourceRows.flatMap((resource) =>
      resource.implementationType === "volume" ? [resource.id] : [],
    ),
  );
  const [showLogs, setShowLogs] = useState(false);
  const queuedForNextTrigger = deployment.status === "queued" && !deployment.dispatchRequestedAt;
  async function deployQueuedTarget() {
    if (!organizationSlug || !queuedForNextTrigger) return;
    setIsDispatching(true);
    try {
      await dispatchQueuedEnvironmentDeploymentServerFn({
          data: {
            organizationSlug,
            projectSlug: deployment.projectSlug,
            environmentSlug: deployment.environmentSlug,
          },
        });
      await reconcileDeploymentCollections(organizationSlug, collectionScope);
      await router.invalidate();
      toast.success("Deployment requested.");
    } catch {
      toast.error("Could not request this deployment.");
    } finally {
      setIsDispatching(false);
    }
  }

  const [cancelOpen, setCancelOpen] = useState(false);

  async function retryDeployment() {
    if (!organizationSlug || !deployment.canRetry) return;
    setIsRetrying(true);
    try {
      await retryEnvironmentDeploymentServerFn({
          data: {
            organizationSlug,
            projectSlug: deployment.projectSlug,
            environmentSlug: deployment.environmentSlug,
            failedDeploymentId: deployment.id,
          },
        });
      await reconcileDeploymentCollections(organizationSlug, collectionScope);
      toast.success("Deployment retry queued.");
    } catch {
      toast.error("Could not retry this deployment.");
    } finally {
      setIsRetrying(false);
    }
  }


  return <>
    <DeploymentStatusCard deployment={deployment} serviceId={serviceId} progress={deployment.runtimeProgress}
      showLogs={showLogs} onLogsChange={setShowLogs}
      logsPanel={<DeploymentLogs organizationSlug={organizationSlug ?? ""} deploymentId={deployment.id} serviceId={serviceId} />} expanded={isOpen} onExpandedChange={setIsOpen} actions={
        <DropdownMenu>
            <DropdownMenuTrigger
              render={<Button variant="ghost" size="icon" />}
            >
            <MoreVerticalIcon />
            <span className="sr-only">Deployment actions</span>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuGroup>
              {organizationSlug && ["queued", "planning", "deploying"].includes(deployment.status) ? (
                <DropdownMenuItem variant="destructive" onClick={() => setCancelOpen(true)}>
                  <XIcon />
                  Cancel deployment
                </DropdownMenuItem>
              ) : null}
              <DropdownMenuItem
                onClick={() =>
                  void navigator.clipboard.writeText(deployment.id)
                }
              >
                Copy deployment ID
              </DropdownMenuItem>
              {deployment.coreDeployId ? (
                <DropdownMenuItem
                  onClick={() =>
                    void navigator.clipboard.writeText(
                      deployment.coreDeployId ?? "",
                    )
                  }
                >
                  Copy core deploy ID
                </DropdownMenuItem>
              ) : null}
              {deployment.canRetry ? (
                <DropdownMenuItem
                  disabled={isRetrying}
                  onClick={() => void retryDeployment()}
                >
                  Retry deployment
                </DropdownMenuItem>
              ) : null}
                {queuedForNextTrigger ? (
                  <DropdownMenuItem
                    disabled={isDispatching}
                    onClick={() => void deployQueuedTarget()}
                  >
                    Deploy now
                  </DropdownMenuItem>
                ) : null}
              {!deployment.canRetry &&
              deployment.status === "failed" &&
              deployment.volumeRemoveAttempts.length > 0 ? (
                <DropdownMenuItem disabled>
                  Review volume deletion from the canvas
                </DropdownMenuItem>
              ) : null}
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      }>
      {deployment.volumeRemoveAttempts.length > 0 ? <div className="px-6 py-3"><VolumeRemoveAttemptHistory
        attempts={deployment.volumeRemoveAttempts} organizationSlug={organizationSlug ?? ""}
        projectSlug={deployment.projectSlug} environmentSlug={deployment.environmentSlug}
        availableVolumeResourceIds={availableVolumeResourceIds}
      /></div> : null}
    </DeploymentStatusCard>
    {organizationSlug ? <CancelDeploymentDialog open={cancelOpen} onOpenChange={setCancelOpen} organizationSlug={organizationSlug} deployment={deployment} /> : null}
  </>;
}
