import { Suspense, type ReactNode } from "react";
import { useMutation, useQueryErrorResetBoundary } from "@tanstack/react-query";
import { toast } from "sonner";
import { authClient } from "#/auth/auth-client";
import { useAuthSession } from "#/auth/auth.hooks";
import { CatchBoundary, useMatch, type ErrorComponentProps } from "@tanstack/react-router";
import { useOrgStoreGate } from "#/collections/org-store";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { AppSidebar } from "./app-sidebar";
import type { DashboardScope } from "./dashboard-navigation-model";
import { DashboardPageHeader } from "./dashboard-header";
import { MobileDashboardNavigation } from "./dashboard-navigation";
import { NavigationProgress } from "./navigation-progress";
import { OrganizationCollectionRefreshNotice } from "./organization-collection-refresh-notice";
import { RouteContentSkeleton } from "./route-content-skeleton";
import { RouteErrorAlert } from "./route-error-alert";
import { SidebarProvider, useSidebar } from "./ui/sidebar";
import { cn } from "#/lib/utils";

export function DashboardShell({
  scope,
  children,
}: {
  scope: DashboardScope;
  children: ReactNode;
}) {
  return <DashboardSidebarProvider><DashboardLayout scope={scope}>{children}</DashboardLayout></DashboardSidebarProvider>;
}

export function DashboardSidebarProvider({ children }: { children: ReactNode }) {
  const { data: auth, refetch } = useAuthSession();
  const preference = useMutation({
    scope: { id: `sidebar-preference:${auth?.session.id}` },
    mutationFn: async (sidebarOpen: boolean) => {
      // Better Auth's client does not infer additional update-session fields yet.
      const result = await authClient.updateSession({ fetchOptions: { method: "POST", body: { sidebarOpen } } });
      if (result.error) throw new Error("Couldn't save sidebar preference. Try again.");
      await refetch();
      if (authClient.$store.atoms["session"]?.get().error) {
        throw new Error("Sidebar preference saved, but session refresh failed. Reload to restore it.");
      }
    },
    onError: (error) => toast.error(error.message),
  });
  const open = preference.isPending
    ? preference.variables
    : auth?.session.sidebarOpen ?? true;

  return (
    <SidebarProvider open={open} onOpenChange={(value) => preference.mutate(value)} className="h-dvh min-h-0 overflow-hidden">
      {children}
    </SidebarProvider>
  );
}

function DashboardLayout({
  scope,
  children,
}: {
  scope: DashboardScope;
  children: ReactNode;
}) {
  const { open, isMobile } = useSidebar();
  const collectionScope = useCollectionScope();
  const canvas = useMatch({
    from: "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
    shouldThrow: false,
  });
  return (
    <>
      {!isMobile ? (
        <aside
          className={cn(
            "group hidden shrink-0 border-r bg-sidebar motion-safe:transition-[width] motion-safe:duration-150 min-wf-nav:block",
            open ? "w-64" : "w-12",
          )}
          data-state={open ? "expanded" : "collapsed"}
          data-collapsible={open ? "" : "icon"}
          aria-label="Dashboard navigation"
        >
          <AppSidebar scope={scope} />
        </aside>
      ) : null}
      <main className="flex min-h-0 min-w-0 flex-1 flex-col bg-background">
        <MobileDashboardNavigation
          key={
            scope.kind === "all"
              ? scope.organizationSlug
              : `${scope.organizationSlug}/${scope.projectSlug}/${scope.environmentSlug}`
          }
          scope={scope}
        />
        {!canvas ? <DashboardPageHeader scope={scope} /> : null}
        <NavigationProgress />
        <OrganizationCollectionRefreshNotice
          scope={collectionScope}
          organizationSlug={scope.organizationSlug}
        />
        <div
          data-scroll-restoration-id="wireframe-content"
          className="min-h-0 min-w-0 flex-1 overflow-y-auto"
        >
          {/* The one Org Store gate: pages below it read org rows synchronously; chrome above it never waits. */}
          <CatchBoundary getResetKey={() => scope.organizationSlug} errorComponent={OrgStoreError}>
            <Suspense fallback={<div className="p-4 md:p-6"><RouteContentSkeleton /></div>}>
              <OrgStoreGate organizationSlug={scope.organizationSlug}>{children}</OrgStoreGate>
            </Suspense>
          </CatchBoundary>
        </div>
      </main>
    </>
  );
}

function OrgStoreGate({ organizationSlug, children }: { organizationSlug: string; children: ReactNode }) {
  useOrgStoreGate(organizationSlug);
  return children;
}

function OrgStoreError({ reset }: ErrorComponentProps) {
  const queries = useQueryErrorResetBoundary();
  return (
    <div className="p-4 md:p-6">
      <RouteErrorAlert
        title="Organization data couldn’t load"
        description="Projects, services, and deployments are unavailable right now. Try loading them again."
        onRetry={() => { queries.reset(); reset(); }}
      />
    </div>
  );
}
