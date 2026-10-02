export const ENVIRONMENT_ROUTE_FROM =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug";

/** The Environment route's params. */
export type EnvironmentRouteParams = { organizationSlug: string; projectSlug: string; environmentSlug: string };

export const ENVIRONMENT_INDEX_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug";

export const ENVIRONMENT_SERVICE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId";

export const ENVIRONMENT_RESOURCE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/resources/$resourceId";

export const ENVIRONMENT_LIVE_NODE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/live/$lineageId";

export const ENVIRONMENT_NEW_BRANCH_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/new-branch";

export const ENVIRONMENT_PR_PLAN_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/pr-environments/$repositoryId";
