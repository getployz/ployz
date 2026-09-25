import type { DeploymentNodeView } from "#/modules/deployments/deployment-view";

/** The Badge variant per outcome. Cards take the colour only for failed and in-flight nodes; a deployed node stays quiet. */
export const outcomeBadges = {
  deployed: "success", removed: "secondary", failed: "destructive", not_attempted: "secondary", unchanged: "secondary",
  queued: "secondary", building: "info", deploying: "info",
} as const satisfies Record<DeploymentNodeView["outcome"], "success" | "secondary" | "destructive" | "info">;
