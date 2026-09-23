import { useId } from "react";
import { Background, BackgroundVariant, ReactFlowProvider } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import type { ServiceSource } from "#/modules/environment-design/services";
import type { RuntimeLensStatus, RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";
import { cn } from "#/lib/utils";

export function ProjectCard({
  name,
  environment,
  runtimeServices,
  runtimeStatus,
}: {
  name: string;
  environment: { name: string; namespace: string; services: { id: string; slug: string; config: { source: ServiceSource } }[] } | null;
  runtimeServices: readonly RuntimeServiceRecord[];
  runtimeStatus: RuntimeLensStatus;
}) {
  const backgroundId = useId();
  const services = environment?.services ?? [];
  const onlineCount = runtimeStatus === "observed"
    ? services.filter(service => runtimeServices.some(runtime =>
      runtime.identity === `${environment?.namespace}/${service.slug}` &&
      runtime.containers.some(container => container.runtime?.state === "running" &&
        (container.runtime.health === "healthy" || container.runtime.health === "not_configured")),
    )).length
    : null;
  const serviceCount = services.length;
  const serviceLabel = serviceCount === 1 ? "service" : "services";

  return (
    <Card className="h-full transition-colors group-hover/project:ring-foreground/30">
      <CardHeader>
        <CardTitle className="truncate" title={name}>{name}</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="react-flow relative isolate flex min-h-56 flex-col overflow-hidden rounded-lg border">
          <ReactFlowProvider>
            <Background id={backgroundId} variant={BackgroundVariant.Dots} gap={16} size={1} />
          </ReactFlowProvider>
          <div className="relative flex flex-1 flex-wrap content-center items-center justify-center gap-3 p-4" aria-label="Services">
            {services.map(service => (
              <span key={service.id} title={service.slug} className="flex size-12 shrink-0 items-center justify-center rounded-lg border bg-card [&_svg]:size-6">
                {getServiceIcon(service.config)}
                <span className="sr-only">{service.slug}</span>
              </span>
            ))}
          </div>
          <div className="relative flex flex-wrap items-center gap-x-2 gap-y-1 p-3 text-xs text-muted-foreground">
            {environment && <>
              <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", onlineCount !== null && onlineCount > 0 ? "bg-success" : "bg-muted-foreground")} />
              <span className="min-w-0 truncate" title={environment.name}>{environment.name.toLowerCase()}</span>
              <span aria-hidden="true">·</span>
            </>}
            <span>{serviceCount === 0 ? "No services" : onlineCount === null
              ? `${serviceCount} ${serviceLabel}`
              : `${onlineCount}/${serviceCount} ${serviceLabel} online`}</span>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
