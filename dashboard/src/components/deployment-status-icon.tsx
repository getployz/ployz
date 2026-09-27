import { CircleCheckIcon, CircleDashedIcon, CircleDotIcon, CircleMinusIcon, CircleSlashIcon, CircleXIcon } from "lucide-react";
import type { DeploymentNodeView, DeploymentViewStatus } from "#/modules/deployments/deployment-view";

/** A whole attempt's status, or one node's outcome in it, as an icon; its label always accompanies it. */
export function DeploymentStatusIcon({ status }: { status: DeploymentViewStatus | DeploymentNodeView["outcome"] }) {
  switch (status) {
    case "deployed": return <CircleCheckIcon className="text-success" />;
    case "failed": return <CircleXIcon className="text-destructive" />;
    case "removed": return <CircleMinusIcon className="text-muted-foreground" />;
    case "cancelled": case "not_attempted": case "unchanged": return <CircleSlashIcon className="text-muted-foreground" />;
    case "queued": return <CircleDashedIcon className="text-muted-foreground" />;
    default: return <CircleDotIcon className="text-info" />;
  }
}
