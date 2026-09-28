import { Suspense, type ReactNode } from "react";
import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { CatchBoundary, Link, type ErrorComponentProps } from "@tanstack/react-router";
import { useOrgStoreGate } from "#/collections/org-store";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getDashboardDestination, getDashboardSectionLabel, type DashboardScope } from "./dashboard-navigation-model";
import { HomeLink, PhoneTabBar, Rail } from "./dashboard-rail";
import { Crumbs, EnvironmentCrumbs } from "./environment-breadcrumbs";
import { NavigationProgress } from "./navigation-progress";
import { OrganizationCollectionRefreshNotice } from "./organization-collection-refresh-notice";
import { RouteContentSkeleton } from "./route-content-skeleton";
import { RouteErrorAlert } from "./route-error-alert";
import { buttonVariants } from "./ui/button-variants";
import { useDashboardNavigation, useDashboardSection, useRouteCrumb } from "./use-dashboard-section";
import DashboardAccountMenu from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardAccountMenu";

export function DashboardShell({
  scope,
  children,
}: {
  scope: DashboardScope;
  children: ReactNode;
}) {
  const collectionScope = useCollectionScope();
  const section = useDashboardSection();
  const Crumb = useRouteCrumb();
  const { places } = useDashboardNavigation(scope);
  return (
    <div className="flex h-dvh min-h-0 overflow-hidden">
      <Rail organizationSlug={scope.organizationSlug} places={places}
        account={<DashboardAccountMenu scope={scope} side="right" />} />
      <main className="flex min-h-0 min-w-0 flex-1 flex-col bg-background">
        {/* The one top bar: on phones it also carries the logo and the avatar, which live in the rail on desktop. */}
        <header className="flex h-12 shrink-0 items-center gap-2 border-b bg-background px-3 min-wf-nav:px-4">
          <HomeLink organizationSlug={scope.organizationSlug} className="min-wf-nav:hidden" />
          {scope.kind === "environment"
            ? <EnvironmentCrumbs scope={scope} />
            : Crumb
              // A detail page declares its crumb: `Servers / hel-1 ⌄`.
              ? <Crumbs items={[
                  <Link key="section" {...getDashboardDestination(scope, section)} className={buttonVariants({ variant: "ghost", size: "sm" })}>
                    {getDashboardSectionLabel(section)}
                  </Link>,
                  <Crumb key="crumb" />,
                ]} />
              : <h1 className="truncate font-semibold">{getDashboardSectionLabel(section)}</h1>}
          <div className="ml-auto min-wf-nav:hidden"><DashboardAccountMenu scope={scope} /></div>
        </header>
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
        <PhoneTabBar places={places} />
      </main>
    </div>
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
