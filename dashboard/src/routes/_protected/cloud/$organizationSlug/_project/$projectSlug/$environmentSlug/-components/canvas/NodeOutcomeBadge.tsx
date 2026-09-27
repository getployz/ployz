import { Badge } from "#/components/ui/badge";
import { Skeleton } from "#/components/ui/skeleton";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { nodeOutcomeLabels } from "#/modules/deployments/deployment-view";
import type { useNodeLighting } from "../deployment-page";

/** A lit node's Node Outcome; until a built node's build tail arrives its outcome is unknown, so claiming one would be false. */
export function NodeOutcomeBadge({ light }: { light: NonNullable<ReturnType<typeof useNodeLighting>> }) {
  return light.pending ? <Skeleton className="h-5 w-16" /> : <Badge variant={outcomeBadges[light.outcome]}>{nodeOutcomeLabels[light.outcome]}</Badge>;
}
