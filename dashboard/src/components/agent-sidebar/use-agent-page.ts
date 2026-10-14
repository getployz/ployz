import { useMemo } from "react";
import { useMatch, useParams } from "@tanstack/react-router";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import type { PageContext } from "#/modules/agent/page-context";
import { servicesQuery, useCachedStoreView } from "#/modules/config-store/store-view.queries";

/**
 * Where the member is, for the agent. The Service is named the way tools take it, read from the Services the Environment
 * route already loaded; until they arrive the page names no Service.
 */
export function useAgentPage(scope: DashboardScope): PageContext {
  const page = useDashboardSection();
  const { serviceId, deploymentId, serverId } = useParams({ strict: false });
  const [project, environment] = scope.kind === "environment" ? [scope.projectSlug, scope.environmentSlug] : [];
  const listed = useCachedStoreView(scope.organizationSlug,
    serviceId !== undefined && project !== undefined && environment !== undefined ? servicesQuery({ project, environment }) : null);
  const environmentId = useMatch({ from: "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug", shouldThrow: false })
    ?.loaderData?.environmentId;
  const environment_id = environment === undefined ? undefined : environmentId;
  const service = listed?.ok ? listed.value.services.find((candidate) => candidate.id === serviceId)?.name : undefined;
  return useMemo(() => ({ page, project, environment, environment_id, service, deployment: deploymentId, server: serverId }),
    [page, project, environment, environment_id, service, deploymentId, serverId]);
}
