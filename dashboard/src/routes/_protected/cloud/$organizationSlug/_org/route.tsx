import { OrganizationCollectionRefreshNotice } from "#/components/organization-collection-refresh-notice";
import { useCollectionScope } from "#/collections/use-collection-scope";
import DashboardHeader from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardHeader";
import {
  WireframeContent,
  Wireframe,
  WireframeNav,
  WireframeSidebar,
} from "#/components/ui/wireframe";
import { SidebarProvider } from "#/components/ui/sidebar";
import { AppSidebar } from "#/components/app-sidebar";
import { NavigationProgress } from "#/components/navigation-progress";
import { createFileRoute, Outlet, useParams } from "@tanstack/react-router";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org")({
  component: RouteComponent,
});

function RouteComponent() {
  const scope = useCollectionScope();
  const params = useParams({ from: "/_protected/cloud/$organizationSlug" });

  return (
    <SidebarProvider className="block h-full min-h-0">
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
              kind: "all",
              organizationSlug: params.organizationSlug,
            }}
          />
        </WireframeSidebar>
        <WireframeNav position="top" className="bg-sidebar">
          <DashboardHeader />
        </WireframeNav>
        <WireframeContent
          className="h-[calc(100dvh-var(--top-nav-height))] overflow-hidden"
          surfaceClassName="flex min-h-0 flex-col overflow-y-auto"
        >
          <NavigationProgress />
          <OrganizationCollectionRefreshNotice scope={scope} organizationSlug={params.organizationSlug} />
          <div className="min-h-0 flex-1"><Outlet /></div>
        </WireframeContent>
      </Wireframe>
    </SidebarProvider>
  );
}
