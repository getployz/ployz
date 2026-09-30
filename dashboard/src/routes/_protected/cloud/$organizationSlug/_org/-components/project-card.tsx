import { useId } from "react";
import { Background, BackgroundVariant, ReactFlowProvider } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import type { RuntimeLensStatus, RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";
import { cn } from "#/lib/utils";
import { servicesOnline } from "../../-components/services-online";

export function ProjectCard({
  name,
  environment,
  runtimeServices,
  runtimeStatus,
}: {
  name: string;
  /** Its Services by name; `slug` is how runtime evidence names each in the Namespace. */
  environment: { name: string; namespace: string; services: { id: string; name: string; slug: string; config: { source: { type: "empty" | "uploaded" | "git" | "image" } } }[] } | null;
  runtimeServices: readonly RuntimeServiceRecord[];
  runtimeStatus: RuntimeLensStatus;
}) {
  const backgroundId = useId();
  const services = environment?.services ?? [];
  const { online, label } = environment ? servicesOnline(environment, runtimeServices, runtimeStatus) : { online: null, label: "No services" };

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
              <span key={service.id} title={service.name} className="flex size-12 shrink-0 items-center justify-center rounded-lg border bg-card [&_svg]:size-6">
                {getServiceIcon(service.config)}
                <span className="sr-only">{service.name}</span>
              </span>
            ))}
          </div>
          <div className="relative flex flex-wrap items-center gap-x-2 gap-y-1 p-3 text-xs text-muted-foreground">
            {environment && <>
              <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", online !== null && online > 0 ? "bg-success" : "bg-muted-foreground")} />
              <span className="min-w-0 truncate" title={environment.name}>{environment.name.toLowerCase()}</span>
              <span aria-hidden="true">·</span>
            </>}
            <span>{label}</span>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
