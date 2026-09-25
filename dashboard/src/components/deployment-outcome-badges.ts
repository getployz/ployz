import type { DeploymentNodeView } from "#/modules/deployments/deployment-view";

/** The Badge variant per outcome; cards take the same colour, except that secondary leaves them plain. */
export const outcomeBadges = {
  deployed: "success", removed: "secondary", failed: "destructive", not_attempted: "secondary", unchanged: "secondary",
  queued: "secondary", building: "info", deploying: "info",
} as const satisfies Record<DeploymentNodeView["outcome"], "success" | "secondary" | "destructive" | "info">;
