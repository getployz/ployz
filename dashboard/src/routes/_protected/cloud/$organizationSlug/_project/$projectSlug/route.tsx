import { createFileRoute, Outlet, redirect } from "@tanstack/react-router";
import { requireStoreProjects } from "#/collections/route-data";
import { renamedProject } from "#/modules/config-store/renamed-projects";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug",
)({
  // Back to a Project this tab renamed opens it under its new name, unless the old name names a Project again.
  beforeLoad: async ({ params, context, location }) => {
    const current = renamedProject(params.organizationSlug, params.projectSlug);
    if (current === undefined) return;
    const projects = await requireStoreProjects(context, params.organizationSlug);
    if (projects.some((project) => project.name === params.projectSlug)) return;
    const from = `/cloud/${params.organizationSlug}/${params.projectSlug}`;
    if (!location.pathname.startsWith(from)) return;
    throw redirect({
      href: `/cloud/${params.organizationSlug}/${current}${location.pathname.slice(from.length)}${location.searchStr}${location.hash ? `#${location.hash}` : ""}`,
      replace: true,
    });
  },
  component: RouteComponent,
});

function RouteComponent() {
  return <Outlet />;
}
