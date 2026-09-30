import type { NodeLight } from "#/modules/config-store/store-deployments";

/** The Badge variant per outcome. Cards take the colour only for failed and in-flight nodes; a deployed node stays quiet. */
export const outcomeBadges = {
  deployed: "success", failed: "destructive", unknown: "warning", not_applied: "secondary", queued: "secondary", deploying: "info",
} as const satisfies Record<NodeLight, "success" | "secondary" | "destructive" | "warning" | "info">;

/** A card's state for its outcome: only failed, unknown and in-flight nodes take a colour. */
export const outcomeCardState = (outcome: NodeLight) => {
  const badge = outcomeBadges[outcome];
  return badge === "destructive" || badge === "warning" || badge === "info" ? badge : undefined;
};
