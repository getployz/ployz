import { createFileRoute, redirect } from "@tanstack/react-router";
import { PREVIEW_ROUTE_TO } from "#/lib/environment-routes";
import { requireOrganization } from "#/modules/organization/organization.route-data";
import { requirePreviewEnvironment } from "#/modules/previews/previews.route-data";

/**
 * The link a pull request's GitHub comment carries, which names no Environment: it opens the preview page from the
 * Environment previews copy. No in-app link may target this URL: on a client navigation this `beforeLoad` reads over
 * the network.
 */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/previews/$repositoryId/$number")({
  beforeLoad: async ({ params, context }) => {
    await requireOrganization(context, params.organizationSlug);
    const environmentSlug = await requirePreviewEnvironment(context, params, params.repositoryId);
    throw redirect({ to: PREVIEW_ROUTE_TO, params: { ...params, environmentSlug }, replace: true });
  },
});
