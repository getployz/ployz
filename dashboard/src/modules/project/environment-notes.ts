import { useLiveQuery } from "@tanstack/react-db";
import { getEnvironmentDeploymentsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useBranchReviews } from "#/modules/branches/use-branch-review";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import type { PrShutdown } from "#/modules/pr-environments/tables";

const SHUTDOWN_NOTE = { running: "shutting down", off: "Off", failed: "shutdown failed" } satisfies Record<PrShutdown, string>;

/**
 * What an Environment has, in a few words, wherever the tree is listed: "PR #142", "Off", "default", "kept", "not deployed",
 * "2 to save", "1 update". A Branch's review is computed only when asked for: core compares it with its Parent.
 */
export function useEnvironmentNotes(organizationSlug: string, defaultEnvironmentId: string | undefined) {
  const { branches } = useWorkspace(organizationSlug);
  const { data: deployments } = useLiveQuery(getEnvironmentDeploymentsCollection(organizationSlug, useCollectionScope()));
  const reviewOf = useBranchReviews(organizationSlug);
  // The Org Store keeps each Environment's latest attempt, so having none means it was never deployed.
  const deployed = new Set(deployments.map((deployment) => deployment.environmentId));
  return (environmentId: string) => {
    const branch = branches.find((row) => row.environmentId === environmentId);
    const review = branch ? reviewOf(environmentId) : null;
    const pullRequest = branch?.pullRequest;
    return [
      pullRequest && `PR #${pullRequest.number}`,
      pullRequest?.shutdown && SHUTDOWN_NOTE[pullRequest.shutdown],
      environmentId === defaultEnvironmentId && "default",
      branch?.kept && "kept",
      !deployed.has(environmentId) && "not deployed",
      !!review?.changes && `${review.changes} to save`,
      !!review?.updates && `${review.updates} ${review.updates === 1 ? "update" : "updates"}`,
    ].filter((note): note is string => typeof note === "string");
  };
}
