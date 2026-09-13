import { OrganizationCollectionRefreshNotice } from "#/components/organization-collection-refresh-notice";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  Wireframe,
  WireframeContent,
  WireframeNav,
  WireframeSidebar,
} from "#/components/ui/wireframe";
import { SidebarProvider } from "#/components/ui/sidebar";
import { AppSidebar } from "#/components/app-sidebar";
import { NavigationProgress } from "#/components/navigation-progress";
import {
  environmentBySlugQueryOptions,
  rememberSelectedEnvironment,
  environmentListQueryOptions,
} from "#/modules/environment-design/workspace-queries";
import { projectBySlugQueryOptions } from "#/modules/environment-design/workspace-queries";
import DashboardHeader from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardHeader";
import {
  createFileRoute,
  notFound,
  Outlet,
  useParams,
} from "@tanstack/react-router";
import { hasPublicErrorCode } from "#/lib/public-error";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    const organizationSlug = params.organizationSlug;
    const navigationReady = Promise.all([
      context.queryClient.prefetchQuery(
        projectBySlugQueryOptions(organizationSlug, params.projectSlug),
      ),
      context.queryClient.prefetchQuery(
        environmentListQueryOptions(
          organizationSlug,
          params.projectSlug,
        ),
      ),
    ]);
    const environment = await context.queryClient
      .ensureQueryData(
        environmentBySlugQueryOptions(
          organizationSlug,
          params.projectSlug,
          params.environmentSlug,
        ),
      )
      .catch((cause: unknown) => {
        if (hasPublicErrorCode(cause, "NOT_FOUND")) throw notFound();
        throw cause;
      });

    return {
      environmentId: environment.id,
      organizationId: environment.organizationId,
      navigationReady,
    };
  },
  onEnter: ({ context, params }) => {
    void rememberSelectedEnvironment(context.queryClient, params);
  },
  onStay: ({ context, params }) => {
    void rememberSelectedEnvironment(context.queryClient, params);
  },
  component: RouteComponent,
});

function RouteComponent() {
  return (
    <SidebarProvider className="block h-full min-h-0">
      <ProjectLayout />
    </SidebarProvider>
  );
}

function ProjectLayout() {
  const scope = useCollectionScope();
  const params = useParams({
    from: "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
  });

  return (
    <Wireframe
      className="overflow-hidden"
      config={{
          cssVariables: {
            "--top-nav-height": "4rem",
            "--left-sidebar-width-expanded": "16rem",
            "--left-sidebar-width-collapsed": "4rem",
          },
        corners: {
          topRight: "navbar",
        },
      }}
    >
      <WireframeSidebar
        position="left"
        className="max-wf-nav:hidden bg-sidebar"
      >
        <AppSidebar
          scope={{
            kind: "environment",
            organizationSlug: params.organizationSlug,
            projectSlug: params.projectSlug,
            environmentSlug: params.environmentSlug,
          }}
        />
      </WireframeSidebar>
      <WireframeNav position="top" className="bg-sidebar">
        <DashboardHeader />
      </WireframeNav>
      <WireframeContent
        className="h-[calc(100dvh-var(--top-nav-height))] min-h-[calc(100dvh-var(--top-nav-height))] overflow-hidden"
        surfaceClassName="flex h-full flex-col overflow-y-auto"
      >
        <NavigationProgress />
        <OrganizationCollectionRefreshNotice scope={scope} organizationSlug={params.organizationSlug} />
          <div className="min-h-0 flex-1"><Outlet /></div>
      </WireframeContent>
    </Wireframe>
  );
}
