import type { NodeLight } from "#/modules/config-store/store-deployments";

/** The Badge variant per outcome: a node's chip under an open Deployment Page, and the page's own badges. */
export const outcomeBadges = {
  deployed: "success", failed: "destructive", unknown: "warning", not_applied: "secondary", queued: "secondary", deploying: "info",
} as const satisfies Record<NodeLight, "success" | "secondary" | "destructive" | "warning" | "info">;

