import { useMatch } from "@tanstack/react-router";
import { liveNodeId } from "./canvas/nodes";

const ENVIRONMENT_SERVICE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId";
const DEPLOYMENT_PAGE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/$deploymentId";
const DEPLOYMENT_LIST_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/";
const NEW_BRANCH_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch";
const LIVE_NODE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/live/$lineageId";
const BRANCH_REVIEW_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/review";
const PR_PLAN_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/pr-environments/$repositoryId";
const ENVIRONMENT_RESOURCE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId";

export type CanvasInspectorSelection = {
  selectedServiceId: string | null;
  selectedResourceId: string | null;
  /** The lineage of the Live Node whose panel is open (its canvas node is `live:<lineage>`). */
  selectedLiveLineageId: string | null;
  /** Id of whichever node's inspector is open, regardless of node type. */
  selectedNodeId: string | null;
  /** The attempt whose Deployment Page is open over the canvas; it opens no node's inspector. */
  deploymentId: string | null;
  /** The service whose Deployments tab opened that Deployment Page, if one did. */
  deploymentReturnTo: string | null;
  /** The Environment's deployment list is open over the canvas. */
  deploymentList: boolean;
  /** The New branch panel is open over the Parent's canvas, opened on `focus` (a lineage) if anything. */
  newBranch: { focus: string | null } | null;
  /** A Branch's review page is open over its canvas. */
  branchReview: boolean;
  /** A repository's PR Environments plan page is open over its start-from Environment's canvas. */
  prPlan: { repositoryId: number } | null;
  isInspectorOpen: boolean;
};

/**
 * Single source of truth for which canvas node's inspector is currently open.
 *
 * Every place that reacts to the inspector (layout, save bar, node centering)
 * should read from here instead of matching routes by hand, so adding a new
 * inspector route type only means editing this hook.
 */
export function useCanvasInspectorSelection(): CanvasInspectorSelection {
  const serviceMatch = useMatch({
    from: ENVIRONMENT_SERVICE_ROUTE_ID,
    shouldThrow: false,
  });
  const resourceMatch = useMatch({
    from: ENVIRONMENT_RESOURCE_ROUTE_ID,
    shouldThrow: false,
  });
  const deploymentMatch = useMatch({
    from: DEPLOYMENT_PAGE_ROUTE_ID,
    shouldThrow: false,
  });
  const deploymentListMatch = useMatch({
    from: DEPLOYMENT_LIST_ROUTE_ID,
    shouldThrow: false,
  });
  const newBranchMatch = useMatch({
    from: NEW_BRANCH_ROUTE_ID,
    shouldThrow: false,
  });
  const liveMatch = useMatch({
    from: LIVE_NODE_ROUTE_ID,
    shouldThrow: false,
  });
  const branchReviewMatch = useMatch({
    from: BRANCH_REVIEW_ROUTE_ID,
    shouldThrow: false,
  });
  const prPlanMatch = useMatch({
    from: PR_PLAN_ROUTE_ID,
    shouldThrow: false,
  });
  const selectedServiceId = serviceMatch?.params.serviceId ?? null;
  const selectedLiveLineageId = liveMatch?.params.lineageId ?? null;
  const selectedResourceId = resourceMatch?.params.resourceId ?? null;
  const selectedNodeId = selectedServiceId ?? selectedResourceId ?? (selectedLiveLineageId && liveNodeId(selectedLiveLineageId));

  return {
    selectedServiceId,
    selectedResourceId,
    selectedLiveLineageId,
    selectedNodeId,
    deploymentId: deploymentMatch?.params.deploymentId ?? null,
    deploymentReturnTo: deploymentMatch?.search.returnTo ?? null,
    deploymentList: deploymentListMatch != null,
    newBranch: newBranchMatch ? { focus: newBranchMatch.search.focus ?? null } : null,
    branchReview: branchReviewMatch != null,
    prPlan: prPlanMatch ? { repositoryId: Number(prPlanMatch.params.repositoryId) } : null,
    isInspectorOpen: selectedNodeId != null,
  };
}
