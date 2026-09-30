import { Badge } from "#/components/ui/badge";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import type { NodeLight } from "#/modules/config-store/store-deployments";

/** A lit node's outcome in the Deployment whose page is open, in the same words as the page. */
export function NodeOutcomeBadge({ light }: { light: { outcome: NodeLight; label: string } }) {
  return <Badge variant={outcomeBadges[light.outcome]}>{light.label}</Badge>;
}
