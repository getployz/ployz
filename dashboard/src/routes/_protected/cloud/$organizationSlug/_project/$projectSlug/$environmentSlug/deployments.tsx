import { createFileRoute, redirect } from "@tanstack/react-router";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "./-components/environment-route-paths";

/** Deployments are a mode of the canvas; old links open the canvas with the deployment list open. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments",
)({
  beforeLoad: ({ params }) => {
    throw redirect({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: { deploymentList: true }, replace: true });
  },
});
