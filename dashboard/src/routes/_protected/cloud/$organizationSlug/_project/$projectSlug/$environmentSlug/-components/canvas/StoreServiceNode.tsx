import { Handle, Position } from "@xyflow/react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import type { ServiceListing } from "@ployz/sdk";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import { outcomeCardState } from "#/components/deployment-outcome-badges";
import { useNodeLighting } from "../deployment-page";
import { NodeOutcomeBadge } from "./NodeOutcomeBadge";
import { useRemoveStoreService } from "../../services/$serviceId/-components/useDeleteService";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";
import { ServiceContextMenu } from "./ServiceContextMenu";
import { PickedNode } from "./PickableNode";
import { useNodePick } from "../new-branch/branch-picking";
import { getServiceIcon, getServiceStatusClasses } from "./service-node-helpers";
import type { StoreCanvasService } from "./types";
import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeService } from "#/providers/runtime-provider";
import { serviceOnline } from "#/routes/_protected/cloud/$organizationSlug/-components/services-online";

/**
 * What a Service's card says, from what the next Deploy does to it; else how it runs (`runtime`, null before any
 * runtime evidence of it).
 */
export function storeServiceStatus(service: ServiceListing, changeCount: number, runtime: RuntimeServiceRecord | null) {
  if (service.change === "create") return { state: "success", text: "Service will be created", badge: "New" } as const;
  if (service.change === "delete") return { state: "destructive", text: "Removed on the next deploy", badge: "Removing" } as const;
  if (service.change === "update") return { state: "changed", text: `${changeCount} ${changeCount === 1 ? "change" : "changes"}`, badge: null } as const;
  if (service.source === "empty") return { state: undefined, text: "Empty", badge: null } as const;
  if (!runtime) return { state: undefined, text: "Deployed", badge: null } as const;
  const containers = `${runtime.containers.length} ${runtime.containers.length === 1 ? "container" : "containers"}`;
  return serviceOnline(runtime)
    ? { state: "success", text: `Online · ${containers}`, badge: null } as const
    : { state: "warning", text: `Not running · ${containers}`, badge: null } as const;
}

/** A Config Store Service on the canvas and in its phone list: opens its drawer, right-click removes it. */
export function StoreServiceCard({ service, subtitle, changeCount, runtimeIdentity, selected, className }: StoreCanvasService & {
  selected: boolean;
  className: string;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const remove = useRemoveStoreService(store, service.name);
  const { runtime } = useRuntimeService(runtimeIdentity ?? "");
  const status = storeServiceStatus(service, changeCount, runtime);
  const dot = getServiceStatusClasses(status.state);
  // Under an open Deployment Page: its Node Outcome, or dimmed when it didn't target this Service.
  const light = useNodeLighting(service.id);

  return (
    <ServiceContextMenu serviceId={service.id} onDelete={remove}>
      <Link
        to={ENVIRONMENT_SERVICE_ROUTE_TO}
        params={{ ...params, serviceId: service.id }}
        search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
        data-canvas-node={service.id}
        aria-current={selected ? "page" : undefined}
        draggable={false}
        className={className}
      >
        <Card size="node" state={light ? outcomeCardState(light.outcome) : status.state}
          className={cn("h-full justify-between", light === null && "opacity-40")} data-selected={selected}>
          <CardHeader>
            <div className="flex items-start gap-3">
              <Avatar><AvatarFallback>{getServiceIcon({ source: { type: service.source } })}</AvatarFallback></Avatar>
              <div className="min-w-0 flex-1 overflow-hidden">
                <CardTitle className="truncate">{service.name}</CardTitle>
                {subtitle ? <CardDescription className="truncate">{subtitle}</CardDescription> : null}
              </div>
              {light ? <NodeOutcomeBadge light={light} /> : status.badge ? <Badge variant={status.badge === "New" ? "success" : "destructive"}>{status.badge}</Badge> : null}
            </div>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-3">
              <span className={cn("flex size-3 items-center justify-center rounded-full", dot.dot)}>
                <span className={cn("size-1.5 rounded-full", dot.innerDot)} />
              </span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">{status.text}</span>
            </div>
          </CardContent>
        </Card>
      </Link>
    </ServiceContextMenu>
  );
}

export function StoreServiceNode({ data, selected }: { data: StoreCanvasService; selected?: boolean }) {
  const pick = useNodePick(data.service.name);
  if (pick) {
    return <PickedNode pick={pick} name={data.service.name} nodeId={data.service.id} icon={getServiceIcon({ source: { type: data.service.source } })} />;
  }
  return (
    <>
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <StoreServiceCard {...data} selected={selected ?? false} className="block h-36 w-72" />
    </>
  );
}
