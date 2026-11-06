import { createFileRoute, Outlet, redirect } from "@tanstack/react-router";
import { requireStoreProjects } from "#/collections/route-data";
import { renamedProject, renamedProjectHref } from "#/modules/config-store/renamed-projects";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug",
)({
  // Back to a Project this tab renamed opens it under its new name, unless the old name names a Project again.
  beforeLoad: async ({ params, context, location }) => {
    if (renamedProject(params.organizationSlug, params.projectSlug) === undefined) return;
    const href = renamedProjectHref(params.organizationSlug, params.projectSlug, location,
      await requireStoreProjects(context, params.organizationSlug));
    if (href) throw redirect({ href, replace: true });
  },
  component: RouteComponent,
});

function RouteComponent() {
  return <Outlet />;
}
