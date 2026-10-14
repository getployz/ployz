import { createFileRoute, redirect } from "@tanstack/react-router";

/** Email footers and older links land here. Account opens inside the Organization the user is acting in. */
export const Route = createFileRoute("/_protected/cloud/account/")({
  beforeLoad: ({ context, cause }) => {
    if (cause === "preload") return;
    const slug = context.session.session.activeOrganizationSlug;
    // Acting in no Organization: Cloud's start page offers one.
    if (!slug) throw redirect({ to: "/cloud", search: {}, replace: true });
    throw redirect({ to: "/cloud/$organizationSlug/~/account", params: { organizationSlug: slug }, replace: true });
  },
});
