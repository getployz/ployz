import { use } from "react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { HardDriveIcon, TriangleAlertIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { Item, ItemActions, ItemContent, ItemGroup, ItemMedia } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import {
  diffQuery, domainsQuery, environmentSettingsQuery, namespaceQuery, requireView, servicesQuery, useInFlightTargets, useStoreViews, volumesQuery,
} from "#/modules/config-store/store-view.queries";
import { useRuntimeService } from "#/providers/runtime-provider";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";
import { deployChip, fillText, fillTone, nodeIssues, runtimeLine, type NodeIssue } from "./node-status";
import { FILL_CLASSES, StatusLine } from "./node-status-view";
import { storeCanvasServices } from "./nodes";
import { RuntimeLensContext, VolumeFillContext, VolumeFillProvider } from "./RuntimeLensProvider";
import type { StoreCanvasService } from "./types";

/** A Service's chip, status line and issues, which its card and its panel both show. */
export function useServiceStatus({ service, domains, changeCount, runtimeIdentity, desiredReplicas, trays }: StoreCanvasService) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { runtime } = useRuntimeService(runtimeIdentity ?? "");
  const lens = use(RuntimeLensContext);
  const fillOf = use(VolumeFillContext);
  const chip = deployChip(service, changeCount, useInFlightTargets(organizationSlug, store));
  const status = runtimeLine(service, runtime, { lens, desiredReplicas, chip });
  const issues = nodeIssues(status, domains, trays.map(({ volume }) => ({ volume, fill: fillOf(volume.id) })));
  return { chip, status, issues };
}

/** What on a Service the user can fix, above its panel's tabs: one line each, with at most one action. Nothing when there is none. */
export function ServiceIssues({ serviceId }: { serviceId: string }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [services, settings, diff, volumes, namespace, domains] = useStoreViews(organizationSlug,
    [servicesQuery(store), environmentSettingsQuery(store), diffQuery(store), volumesQuery(store), namespaceQuery(store), domainsQuery(store)] as const);
  const ns = namespace.ok ? namespace.value.namespace : null;
  const data = storeCanvasServices({
    services: requireView(services).services, settings: requireView(settings), diff: requireView(diff),
    volumes: requireView(volumes).volumes, domains: requireView(domains).domains, namespace: ns,
  }).services.find(({ service }) => service.id === serviceId);
  return data ? <VolumeFillProvider namespace={ns}><IssueList data={data} /></VolumeFillProvider> : null;
}

function IssueList({ data }: { data: StoreCanvasService }) {
  const { issues } = useServiceStatus(data);
  if (issues.length === 0) return null;
  return (
    <ItemGroup aria-label="Issues" className="my-3 gap-1">
      {issues.map((issue) => <IssueRow key={issueKey(issue)} issue={issue} serviceId={data.service.id} />)}
    </ItemGroup>
  );
}

const issueKey = (issue: NodeIssue) =>
  issue.kind === "runtime" ? "runtime" : issue.kind === "domain" ? `domain:${domainName(issue.domain)}` : `volume:${issue.volume.id}`;

const domainName = (domain: Extract<NodeIssue, { kind: "domain" }>["domain"]) =>
  domain.hostname ?? (domain.kind === "generated" ? domain.prefix : "");

const ACTION = buttonVariants({ variant: "outline", size: "xs" });

function IssueRow({ issue, serviceId }: { issue: NodeIssue; serviceId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const service = { ...params, serviceId };
  switch (issue.kind) {
    case "runtime":
      return (
        <Item variant="outline" size="xs">
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
        <Item variant="outline" size="xs">
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
    case "volume":
      return (
        <Item variant="outline" size="xs">
          <ItemMedia variant="icon"><HardDriveIcon className={FILL_CLASSES[fillTone(issue.fill) ?? "warn"].text} /></ItemMedia>
          <ItemContent className={cn("min-w-0 truncate", FILL_CLASSES[fillTone(issue.fill) ?? "warn"].text)}>
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
