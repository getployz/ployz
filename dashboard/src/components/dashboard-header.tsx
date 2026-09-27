import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import {
  getDashboardDestination,
  getDashboardSectionLabel,
  type DashboardScope,
} from "./dashboard-navigation-model";
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbSeparator } from "./ui/breadcrumb";
import { useDashboardSection, useRouteCrumb } from "./use-dashboard-section";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";

function EnvironmentName({
  scope,
}: {
  scope: Extract<DashboardScope, { kind: "environment" }>;
}) {
  const { projects, environments } = useWorkspace(scope.organizationSlug);
  const data = findEnvironment(projects, environments, scope);
  return (
    <span className="truncate text-muted-foreground">
      {data?.name ?? scope.environmentSlug}
    </span>
  );
}

export function DashboardPageHeader({ scope, children }: { scope: DashboardScope; children?: ReactNode }) {
  const section = useDashboardSection();
  const Crumb = useRouteCrumb();
  const label = getDashboardSectionLabel(scope, section);
  return (
    <header
      data-dashboard-header
      className="hidden h-16 shrink-0 items-center gap-4 border-b bg-background px-6 min-wf-nav:flex"
    >
      {Crumb ? (
        <Breadcrumb className="min-w-0">
          <BreadcrumbList className="flex-nowrap">
            <BreadcrumbItem>
              <BreadcrumbLink render={<Link {...getDashboardDestination(scope, section)} />}>{label}</BreadcrumbLink>
            </BreadcrumbItem>
            <BreadcrumbSeparator />
            <BreadcrumbItem className="min-w-0">
              <Crumb />
            </BreadcrumbItem>
          </BreadcrumbList>
        </Breadcrumb>
      ) : (
        <h1 className="truncate font-semibold">{label}</h1>
      )}
      {scope.kind === "environment" ? <EnvironmentName scope={scope} /> : null}
      {children}
    </header>
  );
}
