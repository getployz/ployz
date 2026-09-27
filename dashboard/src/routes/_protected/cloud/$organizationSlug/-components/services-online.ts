import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRuntimeCollections, type RuntimeLensStatus, type RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeStatus } from "#/providers/runtime-provider";

/** Runtime evidence for services-online summaries; incomplete evidence counts as unavailable. */
export function useRuntimeServices(organizationSlug: string) {
  const scope = useCollectionScope();
  const { data: runtimeServices } = useLiveQuery(getRuntimeCollections(organizationSlug, scope).services);
  const { lensStatus, incompleteIds } = useRuntimeStatus();
  const runtimeStatus: RuntimeLensStatus = incompleteIds.machines.length || incompleteIds.containers.length ? "unavailable" : lensStatus;
  return { runtimeServices, runtimeStatus };
}

/**
 * `2/3 services online`: a Service counts once when a running container is healthy or has no health check; hooks never count.
 * `online` is null when runtime evidence is not observed, and the label then shows only the service count.
 */
export function servicesOnline(
  environment: { namespace: string; services: readonly { slug: string }[] },
  runtimeServices: readonly RuntimeServiceRecord[],
  runtimeStatus: RuntimeLensStatus,
) {
  const serviceCount = environment.services.length;
  const online = runtimeStatus === "observed"
    ? environment.services.filter(service => runtimeServices.some(runtime =>
      runtime.identity === `${environment.namespace}/${service.slug}` &&
      runtime.containers.some(container => container.runtime?.state === "running" &&
        (container.runtime.health === "healthy" || container.runtime.health === "not_configured")),
    )).length
    : null;
  const noun = serviceCount === 1 ? "service" : "services";
  const label = serviceCount === 0 ? "No services"
    : online === null ? `${serviceCount} ${noun}` : `${online}/${serviceCount} ${noun} online`;
  return { online, label };
}
