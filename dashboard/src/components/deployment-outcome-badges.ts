import type { NodeLight } from "#/modules/config-store/store-deployments";

/** The Badge variant per outcome. Cards take the colour only for failed and in-flight nodes; a deployed node stays quiet. */
export const outcomeBadges = {
  deployed: "success", failed: "destructive", not_applied: "secondary", queued: "secondary", deploying: "info",
} as const satisfies Record<NodeLight, "success" | "secondary" | "destructive" | "info">;

/** A card's state for its outcome: only failed and in-flight nodes take a colour. */
export const outcomeCardState = (outcome: NodeLight) => {
  const badge = outcomeBadges[outcome];
  return badge === "destructive" || badge === "info" ? badge : undefined;
};
