import { CircleCheckIcon, CircleDashedIcon, CircleDotIcon, CircleSlashIcon, CircleXIcon } from "lucide-react";
import type { DeploymentViewStatus } from "#/modules/deployments/deployment-view";

/** A whole attempt's status as an icon; its label always accompanies it. */
export function DeploymentStatusIcon({ status }: { status: DeploymentViewStatus }) {
  switch (status) {
    case "deployed": return <CircleCheckIcon className="text-success" />;
    case "failed": return <CircleXIcon className="text-destructive" />;
    case "cancelled": return <CircleSlashIcon className="text-muted-foreground" />;
    case "queued": return <CircleDashedIcon className="text-muted-foreground" />;
    default: return <CircleDotIcon className="text-info" />;
  }
}
