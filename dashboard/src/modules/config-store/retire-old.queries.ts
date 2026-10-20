import { useQuery } from "@tanstack/react-query";
import type { DeploymentSummary } from "@ployz/sdk";
import type { DeploymentCleanupView } from "./tables";
import { deploymentCleanupServerFn } from "./retire-old.functions";

const CLEANUP_POLL_MS = 5_000;
/** The cleanup's row arrives a moment after its Deployment ends, so the page keeps asking that long. */
const CLEANUP_ARRIVES_WITHIN_S = 60;

/** Whether the page should ask again for a cleanup that reads `row`: one waiting, or one that may still arrive. */
export function cleanupPending(row: DeploymentCleanupView | null | undefined, deployment: Pick<DeploymentSummary, "in_flight" | "ended_at">, now: number) {
  if (row !== null && row !== undefined) return row.state === "waiting";
  return deployment.in_flight || (deployment.ended_at !== null && now / 1000 - deployment.ended_at < CLEANUP_ARRIVES_WITHIN_S);
}

/** A Cloud Deploy's cleanup of the old copies it replaced, or null for one Cloud never cleaned up. */
export function useDeploymentCleanup(
  organizationSlug: string, deploymentId: string, deployment: Pick<DeploymentSummary, "in_flight" | "ended_at"> | undefined,
) {
  return useQuery({
    queryKey: ["deployment-cleanup", organizationSlug, deploymentId] as const,
    queryFn: () => deploymentCleanupServerFn({ data: { organizationSlug, deploymentId } }),
    staleTime: CLEANUP_POLL_MS,
    refetchInterval: (query) => deployment && cleanupPending(query.state.data, deployment, Date.now()) ? CLEANUP_POLL_MS : false,
  });
}
