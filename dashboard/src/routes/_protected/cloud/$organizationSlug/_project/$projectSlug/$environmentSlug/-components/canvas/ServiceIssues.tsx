import type { DiffView, EnvironmentView, ServiceListing } from "@ployz/sdk";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { HardDriveIcon, TriangleAlertIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { Item, ItemActions, ItemContent, ItemGroup, ItemMedia } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import { domainsQuery, namespaceQuery, requireView, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";
import { fillText, type NodeIssue } from "./node-status";
import { FILL_CLASSES, StatusLine } from "./node-status-view";
import { storeCanvasService, volumeTrays } from "./nodes";
import { useServiceStatus } from "./use-service-status";

/**
 * What on a Service the user can fix, above its panel's tabs: one line each, with at most one action. Nothing when there
 * is none. `settings` and `diff`: the Environment's, as its panel read them.
 */
export function ServiceIssues({ service, settings, diff }: { service: ServiceListing; settings: EnvironmentView; diff: DiffView }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [volumes, namespace, domains] = useStoreViews(organizationSlug, [volumesQuery(store), namespaceQuery(store), domainsQuery(store)] as const);
  const { issues } = useServiceStatus(storeCanvasService(service, {
    settings, diff, domains: requireView(domains).domains, namespace: namespace.ok ? namespace.value.namespace : null,
    trays: volumeTrays([service], requireView(volumes).volumes, diff).trays.get(service.id) ?? [],
  }));
  if (issues.length === 0) return null;
  return (
    <ItemGroup aria-label="Issues" className="my-3">
      {issues.map((issue) => issueRow(issue, params, service.id))}
    </ItemGroup>
  );
}

const domainName = (domain: Extract<NodeIssue, { kind: "domain" }>["domain"]) =>
  domain.hostname ?? (domain.kind === "generated" ? domain.prefix : "");

const ACTION = buttonVariants({ variant: "outline", size: "xs" });

/** An issue's row, keyed by what it's about. */
function issueRow(issue: NodeIssue, params: { organizationSlug: string; projectSlug: string; environmentSlug: string }, serviceId: string) {
  const service = { ...params, serviceId };
  switch (issue.kind) {
    case "runtime":
      return (
        <Item key="runtime" variant="outline" size="xs">
          <ItemContent>
            <div className="flex min-w-0 items-center gap-2">
              <StatusLine status={issue.line} issues={[]} />
              {issue.line.code === null ? null : <span className="shrink-0 text-muted-foreground">exit {issue.line.code}</span>}
            </div>
          </ItemContent>
          <ItemActions>
            <Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={service} search={(prev) => ({ ...prev, tab: "logs" as const })} className={ACTION}>View logs</Link>
          </ItemActions>
        </Item>
      );
    case "domain":
      return (
        <Item key={`domain:${domainName(issue.domain)}`} variant="outline" size="xs">
          <ItemMedia variant="icon"><TriangleAlertIcon className="text-warning" /></ItemMedia>
          <ItemContent className="min-w-0 truncate">{domainName(issue.domain)} needs attention</ItemContent>
          <ItemActions>
            {issue.domain.action?.type === "add_server" ? (
              <Link to="/cloud/$organizationSlug/~/servers" params={params} className={ACTION}>Add a server</Link>
            ) : (
              // Its row in Networking says why, and shows its DNS records when those are the fix.
              <Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={service} search={(prev) => ({ ...prev, tab: "settings" as const })} hash="networking"
                className={ACTION}>
                {issue.domain.action?.type === "dns" ? "DNS records" : "View domain"}
              </Link>
            )}
          </ItemActions>
        </Item>
      );
    case "volume": {
      const toneText = FILL_CLASSES[issue.tone].text;
      return (
        <Item key={`volume:${issue.volume.id}`} variant="outline" size="xs">
          <ItemMedia variant="icon"><HardDriveIcon className={toneText} /></ItemMedia>
          <ItemContent className={cn("min-w-0 truncate", toneText)}>
            {issue.volume.name} is {fillText(issue.fill)}
          </ItemContent>
          {/* Its size is fixed once deployed. */}
          {issue.volume.storage_locked ? null : (
            <ItemActions>
              <Link to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId: issue.volume.id }} className={ACTION}>Resize</Link>
            </ItemActions>
          )}
        </Item>
      );
    }
  }
}
