import { CircleCheckIcon, CircleDashedIcon, CircleDotIcon, CircleHelpIcon, CircleSlashIcon, CircleXIcon } from "lucide-react";
import type { DeploymentLight, NodeLight } from "#/modules/config-store/store-deployments";

/** A Deployment's status, or one node's outcome in it, as an icon; its label always accompanies it. */
export function DeploymentStatusIcon({ status }: { status: DeploymentLight | NodeLight }) {
  switch (status) {
    case "deployed": return <CircleCheckIcon className="text-success" />;
    case "failed": return <CircleXIcon className="text-destructive" />;
    case "unknown": return <CircleHelpIcon className="text-warning" />;
    case "cancelled": case "not_applied": return <CircleSlashIcon className="text-muted-foreground" />;
    case "queued": return <CircleDashedIcon className="text-muted-foreground" />;
    default: return <CircleDotIcon className="text-info" />;
  }
}
