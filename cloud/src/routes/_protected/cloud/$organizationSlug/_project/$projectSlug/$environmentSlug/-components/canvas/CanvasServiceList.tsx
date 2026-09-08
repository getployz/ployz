import { Link, useParams } from "@tanstack/react-router";
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
import type { CanvasServiceState } from "./CanvasServicesContext";
import { useRuntimeService } from "#/providers/runtime-provider";
import {
  getServiceIcon,
  getServiceStatusClasses,
  getServiceSubtitle,
} from "./service-node-helpers";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
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
    latestDeploymentDiffRowCount: serviceState.latestDeploymentDiffRowCount,
    hasRecordedTargetSnapshot: serviceState.hasRecordedTargetSnapshot,
    latestDeploymentStatus: serviceState.latestDeploymentStatus,
  });
  const observedContainers = runtime
    ? `${runtime.containers.length} ${runtime.containers.length === 1 ? "container" : "containers"} observed`
    : null;
  const statusClasses = getServiceStatusClasses(semantics.state);

  return (
    <Link
      to={ENVIRONMENT_SERVICE_ROUTE_TO}
      params={{
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug: params.environmentSlug,
        serviceId: service.id,
      }}
      search={(prev) => prev}
      className="block"
    >
      <Card
        state={semantics.state}
        className={cn("gap-6", selected && "ring-2 ring-ring")}
      >
        <CardHeader>
          <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-3">
            <Avatar>
              <AvatarFallback>{getServiceIcon(service)}</AvatarFallback>
            </Avatar>
            <div className="min-w-0">
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
  );
}

export function CanvasServiceList({
  services,
  selectedServiceId,
  servicesById,
  hasChanges,
}: {
  services: EnvironmentServiceViewRecord[];
  selectedServiceId: string | null;
  servicesById: Map<string, CanvasServiceState>;
  hasChanges: boolean;
}) {
  return (
    <div
      className={cn(
        "absolute inset-0 overflow-y-auto px-4 pb-24 sm:hidden",
        hasChanges ? "pt-24" : "pt-4",
      )}
    >
      <div className="flex flex-col gap-3">
        {services.map((serviceView) => {
          const serviceState = servicesById.get(serviceView.service.id);
          return serviceState ? (
            <ServiceListItem
              key={serviceView.service.id}
              serviceView={serviceView}
              serviceState={serviceState}
              selected={serviceView.service.id === selectedServiceId}
            />
          ) : null;
        })}
      </div>
    </div>
  );
}
