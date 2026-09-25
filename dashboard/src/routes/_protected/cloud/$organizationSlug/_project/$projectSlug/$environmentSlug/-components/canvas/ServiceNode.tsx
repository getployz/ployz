import { Handle, Position } from "@xyflow/react";
import { canvasNodeTransition } from "./constants";
import { ServiceContextMenu } from "./ServiceContextMenu";
import { Link, useParams } from "@tanstack/react-router";
import {
  Avatar,
  AvatarFallback,
} from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { getServiceDeploymentSemantics } from "#/modules/services/service-deployment-semantics";
import { useRuntimeService } from "#/providers/runtime-provider";
import { cn } from "#/lib/utils";
import { useCanvasService } from "./CanvasServicesContext";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "../environment-route-paths";
import type { CanvasServiceNodeData } from "./types";
import {
  getServiceIcon,
  getServiceStatusClasses,
  getServiceSubtitle,
} from "./service-node-helpers";

export function LoadingNode() {
  return (
    <Card size="node" className="h-36 w-72 justify-between">
      <CardHeader>
        <div className="flex items-start gap-3">
          <Skeleton className="size-8" />
          <div className="min-w-0 flex-1">
            <Skeleton className="h-5 w-28" />
            <Skeleton className="mt-1.5 h-4 w-36" />
          </div>
        </div>
      </CardHeader>
      <CardContent>
        <div className="flex items-center gap-3">
          <Skeleton className="size-3" />
          <Skeleton className="h-4 w-28" />
        </div>
      </CardContent>
    </Card>
  );
}

export function ServiceNode({
  data,
  selected,
}: {
  data: CanvasServiceNodeData;
  selected?: boolean;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const serviceState = useCanvasService(data.serviceId);
  const runtimeIdentity = serviceState
    ? `${serviceState.serviceView.service.environmentSlug}/${serviceState.serviceView.service.privateDns}`
    : "";
  const { runtime } = useRuntimeService(runtimeIdentity);

  if (!serviceState) {
    return <LoadingNode />;
  }

  const serviceView = serviceState.serviceView;
  const service = serviceView.service;
  const subtitle = getServiceSubtitle(service);
  const hasBeenDeployed = service.firstDeployedAt != null;
  const semantics = getServiceDeploymentSemantics({
    isEmpty: service.source.type === "empty",
    hasBeenDeployed,
    currentDiffRowCount: serviceState.diffRowCount,
    hasRecordedTargetSnapshot: serviceState.hasRecordedTargetSnapshot,
    latestDeploymentStatus: serviceState.latestDeploymentStatus,
  });
  const state = semantics.state;
  const observedContainers = runtime
    ? `${runtime.containers.length} ${runtime.containers.length === 1 ? "container" : "containers"} observed`
    : null;
  const statusCopy = observedContainers
    ? `${semantics.statusText} · ${observedContainers}`
    : semantics.statusText;
  const statusClasses = getServiceStatusClasses(state);

  return (
    <ServiceContextMenu serviceId={service.id}>
      <Link
        to={ENVIRONMENT_SERVICE_ROUTE_TO}
        params={{
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
          serviceId: service.id,
        }}
        search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
        data-canvas-node={service.id}
        {...canvasNodeTransition(service.id)}
        preload="intent"
        draggable={false}
        className="block h-36 w-72"
      >
        <Handle
          type="target"
          position={Position.Bottom}
          isConnectable={false}
          className="opacity-0"
        />
        <Handle
          type="source"
          position={Position.Top}
          isConnectable={false}
          className="opacity-0"
        />
        <Card
          size="node"
          state={state}
          className="h-full justify-between"
          data-selected={selected}
        >
          <CardHeader>
            <div className="flex items-start gap-3">
              <Avatar>
                <AvatarFallback>
                  {getServiceIcon(service)}
                </AvatarFallback>
              </Avatar>
              <div className="min-w-0 flex-1 overflow-hidden">
                <CardTitle>
                  {service.name}
                </CardTitle>
                {subtitle ? (
                  <CardDescription>
                    {subtitle}
                  </CardDescription>
                ) : null}
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {semantics.showNewBadge ? (
                  <Badge variant="success">New</Badge>
                ) : null}
              </div>
            </div>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-3">
              <span
                className={cn(
                  "flex size-3 items-center justify-center rounded-full",
                  statusClasses.dot,
                )}
              >
                <span className={cn("size-1.5 rounded-full", statusClasses.innerDot)} />
              </span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                {statusCopy}
              </span>
            </div>
          </CardContent>
        </Card>
      </Link>
    </ServiceContextMenu>
  );
}
