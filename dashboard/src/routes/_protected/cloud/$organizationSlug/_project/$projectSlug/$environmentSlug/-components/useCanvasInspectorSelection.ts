import { useMatch } from "@tanstack/react-router";

const ENVIRONMENT_SERVICE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId";
const DEPLOYMENT_PAGE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/$deploymentId";
const DEPLOYMENT_LIST_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/";
const NEW_BRANCH_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch";
const BRANCH_REVIEW_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/review";
const ENVIRONMENT_RESOURCE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId";

export type CanvasInspectorSelection = {
  selectedServiceId: string | null;
  selectedResourceId: string | null;
  /** Id of whichever node's inspector is open, regardless of node type. */
  selectedNodeId: string | null;
  /** The attempt whose Deployment Page is open over the canvas; it opens no node's inspector. */
  deploymentId: string | null;
  /** The service whose Deployments tab opened that Deployment Page, if one did. */
  deploymentReturnTo: string | null;
  /** The Environment's deployment list is open over the canvas. */
  deploymentList: boolean;
  /** The New branch panel is open over the Parent's canvas. */
  newBranch: boolean;
  /** A Branch's review page is open over its canvas. */
  branchReview: boolean;
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
  const branchReviewMatch = useMatch({
    from: BRANCH_REVIEW_ROUTE_ID,
    shouldThrow: false,
  });
  const selectedServiceId = serviceMatch?.params.serviceId ?? null;
  const selectedResourceId = resourceMatch?.params.resourceId ?? null;
  const selectedNodeId = selectedServiceId ?? selectedResourceId;

  return {
    selectedServiceId,
    selectedResourceId,
    selectedNodeId,
    deploymentId: deploymentMatch?.params.deploymentId ?? null,
    deploymentReturnTo: deploymentMatch?.search.returnTo ?? null,
    deploymentList: deploymentListMatch != null,
    newBranch: newBranchMatch != null,
    branchReview: branchReviewMatch != null,
    isInspectorOpen: selectedNodeId != null,
  };
}
