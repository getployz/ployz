export const ENVIRONMENT_ROUTE_FROM =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug";

export const ENVIRONMENT_INDEX_ROUTE_FROM =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/";

export const ENVIRONMENT_INDEX_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug";

export const ENVIRONMENT_SERVICE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId";

export const ENVIRONMENT_RESOURCE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/resources/$resourceId";

/** The canvas search key that puts it into Deployment Mode for one attempt. */
export const DEPLOYMENT_SEARCH_KEY = "deployment";

/** The attempt a location's search string shows in Deployment Mode; null in Live Mode. */
export const shownDeployment = (searchStr: string) => new URLSearchParams(searchStr).get(DEPLOYMENT_SEARCH_KEY);
