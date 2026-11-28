export const ENVIRONMENT_ROUTE_FROM =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug";

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


export const ENVIRONMENT_DEPLOYMENTS_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments";

export const DEPLOYMENT_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId";

export const VERSIONS_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/versions";

/** A repository's pull requests and their previews, from an Environment. */
export const PREVIEWS_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/previews/$repositoryId";

export const PREVIEW_SETTINGS_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/previews/$repositoryId/settings";

export const PREVIEW_CLOSE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/previews/$repositoryId/close";

/** One pull request's preview: what it is when live, else what deploys. */
export const PREVIEW_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/previews/$repositoryId/$number";

export type PreviewRouteParams = EnvironmentRouteParams & { repositoryId: string };
