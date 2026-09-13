import { useState } from "react";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { reconcileDeploymentCollections } from "#/modules/deployments/deployment-collection";
import { cancelEnvironmentDeploymentServerFn } from "#/modules/deployments/deployment.functions";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { AlertDialog, AlertDialogContent, AlertDialogHeader, AlertDialogTitle, AlertDialogDescription, AlertDialogFooter, AlertDialogCancel } from "#/components/ui/alert-dialog";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";

export function CancelDeploymentDialog({ open, onOpenChange, organizationSlug, deployment }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  deployment: EnvironmentDeploymentSummary;
}) {
  const [pending, setPending] = useState(false);
  const scope = useCollectionScope();
  const active = !deployment.cancellationRequestedAt && ["queued", "planning", "deploying"].includes(deployment.status);

  async function cancel() {
    if (pending || !active) return;
    setPending(true);
    try {
      await cancelEnvironmentDeploymentServerFn({ data: {
        organizationSlug, projectSlug: deployment.projectSlug,
        environmentSlug: deployment.environmentSlug, deploymentId: deployment.id,
      } });
      onOpenChange(false);
      toast.success("Cancellation requested.");
      await reconcileDeploymentCollections(organizationSlug, scope);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Could not cancel deployment.");
    } finally {
      setPending(false);
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={(next) => { if (!pending) onOpenChange(next); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Cancel deployment?</AlertDialogTitle>
          <AlertDialogDescription>
            {deployment.status === "queued"
              ? "This deployment will be removed from the queue."
              : "The deployment stays active until the runtime finishes cancelling and cleaning up. Completed changes will not be undone."}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>Keep deploying</AlertDialogCancel>
          <Button variant="destructive" disabled={pending || !active} onClick={() => void cancel()}>
            {pending ? <Spinner /> : null}
            {pending ? "Cancelling…" : "Cancel deployment"}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
