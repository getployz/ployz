import type { ReactNode } from "react";
import {
  getDashboardSectionLabel,
  type DashboardScope,
} from "./dashboard-navigation-model";
import { useDashboardSection } from "./use-dashboard-section";
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
  return (
    <header
      data-dashboard-header
      className="hidden h-16 shrink-0 items-center gap-4 border-b bg-background px-6 min-wf-nav:flex"
    >
      <h1 className="truncate font-semibold">{getDashboardSectionLabel(scope, section)}</h1>
      {scope.kind === "environment" ? <EnvironmentName scope={scope} /> : null}
      {children}
    </header>
  );
}
