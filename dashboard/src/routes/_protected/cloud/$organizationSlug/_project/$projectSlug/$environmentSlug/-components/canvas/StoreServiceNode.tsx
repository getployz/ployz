import { type MouseEvent, type ReactNode, Suspense, use, useId } from "react";
import type { DomainRow } from "@ployz/sdk";
import { Handle, Position } from "@xyflow/react";
import { CatchBoundary, Link, useHydrated, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { LockIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import { useRemoveStoreService } from "../../_canvas/services/$serviceId/-components/useRemoveStoreService";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "#/lib/environment-routes";
import { ServiceContextMenu } from "./ServiceContextMenu";
import { PickedNode } from "./PickableNode";
import { useNodePick } from "../new-branch/branch-picking";
import { getServiceIcon } from "./service-node-helpers";
import { DeployChip, STAGED_CLASSES, StatusLine } from "./node-status-view";
import { chipWord, deployChip, deployEffect, issuesLabel, nodeIssues, nodeLabel, notSetLabel, publicDomain, runningDeployment, runtimeLine, stagedChip, stagedSurface, statusWord } from "#/modules/runtime/node-status";
import { RuntimeLensContext } from "./RuntimeLensProvider";
import { ServiceTrays } from "./VolumeTray";
import { CopyHang } from "./MirrorNode";
import type { VolumeCopies } from "#/modules/volume-copies/volume-copies";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import type { StoreCanvasService } from "./types";
import { deploymentQuery, domainsQuery, useCachedStoreRead, useCachedStoreView, useInFlightDeployments, useStoreView } from "#/modules/config-store/store-view.queries";
import { useRuntimeService } from "#/providers/runtime-provider";
import { useUnreachableHost } from "#/modules/uptime/uptime.queries";
import { useContainerTrouble } from "./use-container-trouble";
import { usePendingNode } from "./usePendingNode";

/**
 * A Config Store Service on the canvas and in its phone list: what runs now on its status line, anything about Deploys
 * in its chip. Opens its drawer; right-click removes it. `compact`: the phone list's two rows.
 */
export function StoreServiceCard({ service, changeCount, runtimeIdentity, desiredReplicas, notSet, selected, compact = false, className }:
  StoreCanvasService & { selected: boolean; compact?: boolean; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store, environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const remove = useRemoveStoreService(store, service);
  const { runtime } = useRuntimeService(runtimeIdentity ?? "");
  const runtimeLens = use(RuntimeLensContext);
  const inFlight = useInFlightDeployments(params.organizationSlug, store);
  const { name, leaving } = usePendingNode("service", service.name);
  const running = runningDeployment(inFlight);
  const view = useCachedStoreRead(params.organizationSlug, running === null ? null : deploymentQuery(running.id));
  const deploy = deployEffect(service.name, running, view);
  const chip = deployChip(service, changeCount, inFlight, deploy);
  const status = runtimeLine(service, runtime, { lens: runtimeLens, desiredReplicas, deploy });
  const scope = { organizationSlug: params.organizationSlug, environmentId };
  // Uptime streams too: the server rendered this card before it landed, so the render that hydrates it shows none either.
  const hydrated = useHydrated();
  const incident = useUnreachableHost(scope, service.id);
  const unreachable = hydrated ? incident : null;
  const trouble = useContainerTrouble(scope, service.name);
  const surface = stagedSurface(service.change);
  const icon = <Avatar><AvatarFallback>{getServiceIcon({ source: { type: service.source } })}</AvatarFallback></Avatar>;
  const title = <CardTitle className={cn("truncate", surface && STAGED_CLASSES[surface].name)}>{name}</CardTitle>;
  const subtitle = (
    <ServiceDomains service={service.name}>
      {(domains) => {
        const domain = publicDomain(domains);
        return domain ? <CardDescription className={cn("truncate", domain.live && "text-foreground")}>{domain.hostname}</CardDescription> : null;
      }}
    </ServiceDomains>
  );
  // A Deploy in flight takes the chip, so staged work beside it keeps its own word on the pink card.
  const staged = service.change === null || chip?.kind === "staged" ? null : stagedChip(service.change, changeCount);
  const chips = chip === null && staged === null ? null : <><DeployChip chip={chip} /><DeployChip chip={staged} /></>;
  const navigate = useNavigate();
  // Secrets it has no value for: deploys skip them, so the card counts them and opens its variables.
  const unset = notSetLabel(notSet);
  const notSetChip = unset === undefined ? null : (
    <span role="link" className="flex shrink-0 items-center gap-1 text-xs text-warning hover:underline"
      onClick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        void navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: service.id }, search: { tab: "variables" } });
      }}>
      <LockIcon className="size-3" />{unset}
    </span>
  );

  // An open uptime incident, or an OOM kill or restart: the ⚠ opens Monitoring, where the evidence and the fix are.
  const openMonitoring = (event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    void navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: service.id }, search: { tab: "monitoring" } });
  };
  // The card's name says its domain and the issues they add, so it's written where the domains land.
  const labelId = useId();
  const statusLine = (lineClassName?: string) => (
    <ServiceDomains service={service.name}>
      {(domains) => {
        const issues = nodeIssues(status, domains, { unreachable, trouble });
        return <>
          <span id={labelId} hidden>
            {nodeLabel(name, publicDomain(domains)?.hostname, chipWord(chip), chipWord(staged), statusWord(status), issues && issuesLabel(issues), unset)}
          </span>
          <StatusLine status={status} issues={issues} onIssues={issues?.monitoring ? openMonitoring : undefined} className={lineClassName} />
        </>;
      }}
    </ServiceDomains>
  );

  return (
    <ServiceContextMenu serviceId={service.id} onDelete={remove}>
      <Link
        to={ENVIRONMENT_SERVICE_ROUTE_TO}
        params={{ ...params, serviceId: service.id }}
        search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
        data-canvas-node={service.id}
        aria-labelledby={labelId}
        aria-current={selected ? "page" : undefined}
        draggable={false}
        className={cn("relative z-10 rounded-xl", className)}
      >
        <Card size={compact ? "sm" : "node"} state={surface}
          className={cn("h-full justify-between", leaving && "opacity-40")} data-leaving={leaving || undefined} data-selected={selected} data-down={status.down}>
          {compact ? (
            <CardContent className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3">
              <div className="row-span-2">{icon}</div>
              {title}
              <div className="flex flex-col items-end gap-1 justify-self-end">{chips}</div>
              <div className="min-w-0">{subtitle}</div>
              {statusLine("justify-self-end")}
            </CardContent>
          ) : <>
            <CardHeader>
              <div className="flex items-start gap-3">
                {icon}
                <div className="min-w-0 flex-1 overflow-hidden">{title}{subtitle}</div>
              </div>
              {/* A row of their own, so a running Deploy's clock never squeezes a long name. */}
              {chips === null ? null : <div className="flex flex-wrap gap-1">{chips}</div>}
            </CardHeader>
            <CardContent className="flex items-center justify-between gap-2">{statusLine()}{notSetChip}</CardContent>
          </>}
        </Card>
      </Link>
    </ServiceContextMenu>
  );
}

type DomainsProps = { service: string; children: (domains: readonly DomainRow[]) => ReactNode };

/**
 * Draws the parts of a Service's card that show its domains. Domains wait on the Cluster, so only these parts suspend
 * and draw without them meanwhile; the card, its link and its menu stay put when they land.
 */
function ServiceDomains({ service, children }: DomainsProps) {
  return (
    <CatchBoundary getResetKey={() => service} errorComponent={() => <CachedDomains service={service}>{children}</CachedDomains>}>
      <Suspense fallback={children([])}>
        <LiveDomains service={service}>{children}</LiveDomains>
      </Suspense>
    </CatchBoundary>
  );
}

function LiveDomains({ service, children }: DomainsProps) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useStoreView(organizationSlug, domainsQuery(store));
  // A closed Branch's views refuse as it leaves; the canvas stops drawing, and meanwhile the card shows no domains.
  return children(result.ok ? result.value.domains.filter((domain) => domain.service === service) : []);
}

/** After a failed read the card shows no domains, and shows them again once a later read of the cache answers. */
function CachedDomains({ service, children }: DomainsProps) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useCachedStoreView(organizationSlug, domainsQuery(store));
  return children(result?.ok ? result.value.domains.filter((domain) => domain.service === service) : []);
}

export function StoreServiceNode({ data }: { data: StoreCanvasService & { hang: readonly VolumeCopies[] } }) {
  const pick = useNodePick(data.service.name);
  const { selectedNodeId } = useCanvasInspectorSelection();
  const trays = <>
    <ServiceTrays trays={data.trays} configTrays={data.configTrays} selectedNodeId={selectedNodeId} />
    <CopyHang hang={data.hang} />
  </>;
  if (pick) {
    return <>
      <div className="relative z-10">
        <PickedNode pick={pick} name={data.service.name} nodeId={data.service.id} icon={getServiceIcon({ source: { type: data.service.source } })} />
      </div>
      {trays}
    </>;
  }
  return (
    <>
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <StoreServiceCard {...data} selected={selectedNodeId === data.service.id} className="block h-36 w-72" />
      {trays}
    </>
  );
}
