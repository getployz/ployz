import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { Route as AllOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_org/~/index";

export const Route = createFileRoute("/_protected/cloud/")({
  beforeLoad: ({ context, cause }) => {
    if (cause === "preload") return;

    const slug = context.session.session.activeOrganizationSlug;

    if (!slug) {
      throw notFound();
    }

    throw redirect({
      to: AllOverviewRoute.to,
      replace: true,
      params: { organizationSlug: slug },
    });
  },
});
