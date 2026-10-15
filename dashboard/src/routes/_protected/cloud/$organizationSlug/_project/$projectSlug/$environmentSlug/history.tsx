import { createFileRoute, redirect } from "@tanstack/react-router";
import { VERSIONS_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/run-search";

/** Versions lived here over the canvas; GitHub checks already posted still link it. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/history")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: VERSIONS_ROUTE_TO, params, replace: true });
  },
});
