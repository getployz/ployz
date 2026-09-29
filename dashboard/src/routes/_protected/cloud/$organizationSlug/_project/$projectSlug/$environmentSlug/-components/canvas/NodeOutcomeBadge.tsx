import { Badge } from "#/components/ui/badge";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { nodeLightLabels, type NodeLight } from "#/modules/config-store/store-deployments";

/** A lit node's outcome in the Deployment whose page is open. */
export function NodeOutcomeBadge({ light }: { light: { outcome: NodeLight } }) {
  return <Badge variant={outcomeBadges[light.outcome]}>{nodeLightLabels[light.outcome]}</Badge>;
}
