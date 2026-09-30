import { use } from "react";
import { Handle, Position } from "@xyflow/react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { useRemoveStoreService } from "../../services/$serviceId/-components/useDeleteService";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";
import { ServiceContextMenu } from "./ServiceContextMenu";
import { PickedNode } from "./PickableNode";
import { useNodePick } from "../new-branch/branch-picking";
import { getServiceIcon } from "./service-node-helpers";
import { DeployChip, STAGED_CLASSES, StatusLine } from "./node-status-view";
import { deployChip, nodeIssues, publicDomain, runtimeLine, stagedSurface } from "./node-status";
import { RuntimeLensContext } from "./RuntimeLensProvider";
import { ServiceTrays } from "./VolumeTray";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import type { StoreCanvasService } from "./types";
import { useInFlightDeployments } from "#/modules/config-store/store-view.queries";
import { useRuntimeService } from "#/providers/runtime-provider";

/**
 * A Config Store Service on the canvas and in its phone list: what runs now on its status line, anything about Deploys
 * in its chip. Opens its drawer; right-click removes it. `compact`: the phone list's two rows.
 */
export function StoreServiceCard({ service, domains, changeCount, runtimeIdentity, desiredReplicas, selected, compact = false, className }:
  StoreCanvasService & { selected: boolean; compact?: boolean; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const remove = useRemoveStoreService(store, service);
  const { runtime } = useRuntimeService(runtimeIdentity ?? "");
  const runtimeLens = use(RuntimeLensContext);
  const inFlight = useInFlightDeployments(params.organizationSlug, store);
  // Under an open Deployment Page: its Node Outcome, or dimmed when it didn't target this Service.
  const light = useNodeLighting(service.id);
  const chip = deployChip(service, changeCount, inFlight);
  const status = runtimeLine(service, runtime, { lens: runtimeLens, desiredReplicas, deploying: chip?.kind === "deploying" });
  const issues = nodeIssues(status, domains);
  const domain = publicDomain(domains);
  const surface = stagedSurface(light, service.change);
  const icon = <Avatar><AvatarFallback>{getServiceIcon({ source: { type: service.source } })}</AvatarFallback></Avatar>;
  const title = <CardTitle className={cn("truncate", surface && STAGED_CLASSES[surface].name)}>{service.name}</CardTitle>;
  const subtitle = domain ? <CardDescription className={cn("truncate", domain.live && "text-foreground")}>{domain.hostname}</CardDescription> : null;
  const chipBadge = <DeployChip light={light} chip={chip} />;

  return (
    <ServiceContextMenu serviceId={service.id} onDelete={remove}>
      <Link
        to={ENVIRONMENT_SERVICE_ROUTE_TO}
        params={{ ...params, serviceId: service.id }}
        search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
        data-canvas-node={service.id}
        aria-current={selected ? "page" : undefined}
        draggable={false}
        className={cn("relative z-10 rounded-xl", className)}
      >
        <Card size={compact ? "sm" : "node"} state={surface}
          className={cn("h-full justify-between", light === null && "opacity-40")} data-selected={selected} data-down={status.down}>
          {compact ? (
            <CardContent className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3">
              <div className="row-span-2">{icon}</div>
              {title}
              <div className="justify-self-end">{chipBadge}</div>
              <div className="min-w-0">{subtitle}</div>
              <StatusLine status={status} issues={issues} className="justify-self-end" />
            </CardContent>
          ) : <>
            <CardHeader>
              <div className="flex items-start gap-3">
                {icon}
                <div className="min-w-0 flex-1 overflow-hidden">{title}{subtitle}</div>
                {chipBadge}
              </div>
            </CardHeader>
            <CardContent><StatusLine status={status} issues={issues} /></CardContent>
          </>}
        </Card>
      </Link>
    </ServiceContextMenu>
  );
}

export function StoreServiceNode({ data }: { data: StoreCanvasService }) {
  const pick = useNodePick(data.service.name);
  const { selectedNodeId } = useCanvasInspectorSelection();
  const trays = <ServiceTrays trays={data.trays} selectedNodeId={selectedNodeId} />;
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
