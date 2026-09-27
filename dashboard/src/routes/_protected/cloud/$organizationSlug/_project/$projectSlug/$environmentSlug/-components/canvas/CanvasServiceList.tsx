import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon } from "lucide-react";
import { ServiceContextMenu } from "./ServiceContextMenu";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { getServiceDeploymentSemantics } from "#/modules/services/service-deployment-semantics";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import type {
  CanvasServiceState,
  CanvasVolumeResourceState,
} from "./CanvasServicesContext";
import { useRuntimeService } from "#/providers/runtime-provider";
import {
  getServiceIcon,
  getServiceStatusClasses,
  getServiceSubtitle,
} from "./service-node-helpers";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
  ENVIRONMENT_RESOURCE_ROUTE_TO,
} from "../environment-route-paths";
import { cn } from "#/lib/utils";

function ServiceListItem({
  serviceView,
  serviceState,
  selected,
}: {
  serviceView: EnvironmentServiceViewRecord;
  serviceState: CanvasServiceState;
  selected: boolean;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const service = serviceView.service;
  const { runtime } = useRuntimeService(
    `${service.environmentSlug}/${service.privateDns}`,
  );
  const subtitle = getServiceSubtitle(service);
  const semantics = getServiceDeploymentSemantics({
    isEmpty: service.source.type === "empty",
    hasBeenDeployed: service.firstDeployedAt != null,
    currentDiffRowCount: serviceState.diffRowCount,
    hasRecordedTargetSnapshot: serviceState.hasRecordedTargetSnapshot,
    latestDeploymentStatus: serviceState.latestDeploymentStatus,
  });
  const observedContainers = runtime
    ? `${runtime.containers.length} ${runtime.containers.length === 1 ? "container" : "containers"} observed`
    : null;
  const statusClasses = getServiceStatusClasses(semantics.state);

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
        aria-current={selected ? "page" : undefined}
        className="block"
      >
        <Card
          state={semantics.state}
          data-selected={selected}
        >
          <CardHeader>
            <div className="flex items-start gap-3">
              <Avatar>
                <AvatarFallback>{getServiceIcon(service)}</AvatarFallback>
              </Avatar>
              <div className="min-w-0 flex-1">
                <CardTitle className="truncate">{service.name}</CardTitle>
                {subtitle ? (
                  <CardDescription className="truncate">{subtitle}</CardDescription>
                ) : null}
              </div>
              {semantics.showNewBadge ? <Badge variant="success">New</Badge> : null}
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
                {semantics.statusText}
                {observedContainers ? ` · ${observedContainers}` : null}
              </span>
            </div>
          </CardContent>
        </Card>
      </Link>
    </ServiceContextMenu>
  );
}

export function CanvasNodeList({
  services,
  selectedNodeId,
  servicesById,
  volumeResourcesById,
}: {
  services: EnvironmentServiceViewRecord[];
  selectedNodeId: string | null;
  servicesById: Map<string, CanvasServiceState>;
  volumeResourcesById: Map<string, CanvasVolumeResourceState>;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {services.map((serviceView) => {
          const serviceState = servicesById.get(serviceView.service.id);
          return serviceState ? (
            <ServiceListItem
              key={serviceView.service.id}
              serviceView={serviceView}
              serviceState={serviceState}
              selected={serviceView.service.id === selectedNodeId}
            />
          ) : null;
        })}
        {[...volumeResourcesById.values()].map(({ resource, diffRowCount }) => {
          const removed = !resource.isAuthored;
          const summary = resource.attachments.map((attachment) => attachment.mountPath).join(", ") || "No mounts";
          const selected = resource.resource.id === selectedNodeId;
          return <Link
            key={resource.resource.id}
            to={ENVIRONMENT_RESOURCE_ROUTE_TO}
            params={{ ...params, resourceId: resource.resource.id }}
            search={(previous) => ({ ...previous, tab: selected ? previous.tab : undefined })}
            className="block"
            data-canvas-node={resource.resource.id}
            aria-current={selected ? "page" : undefined}
          >
            <Card state={removed ? "destructive" : diffRowCount > 0 ? "changed" : undefined} data-selected={selected}>
              <CardHeader>
                <div className="flex items-start gap-3">
                  <Avatar><AvatarFallback><HardDriveIcon /></AvatarFallback></Avatar>
                  <div className="min-w-0 flex-1">
                    <CardTitle className="truncate" title={resource.resource.name}>{resource.resource.name}</CardTitle>
                    <CardDescription>Volume</CardDescription>
                  </div>
                  {removed ? <Badge variant="destructive">Removing</Badge> : diffRowCount > 0 ? <Badge variant="changed">{diffRowCount}</Badge> : null}
                </div>
              </CardHeader>
              <CardContent><p className="truncate text-muted-foreground" title={summary}>{summary}</p></CardContent>
            </Card>
          </Link>;
        })}
      </div>
    </div>
  );
}
